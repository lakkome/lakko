// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The recovery email ([ADR-0075]; UX `docs/ux/identity-recovery.md` section
//! 19): a code mailed to an address the person added brings them back in.
//!
//! # What the server keeps
//!
//! Never the address (invariant #1). For an account that added one:
//!
//! ```text
//! email_key = keyed-BLAKE3(LAKKO_AUTH_EMAIL_PEPPER,
//!     "lakko/auth/recovery-email/v1" ‖ login_handle(32) ‖ len(2 BE) ‖ address)
//! ```
//!
//! over the normalised address ([`normalise_address`]). With the database and
//! the pepper a suspected (username, address) pair can be confirmed; without
//! the pepper it cannot.
//!
//! No vault: the email vault this module once kept (the phrase entropy
//! sealed under a key the operator could reach) was removed (ADR-0075,
//! amended 2026-11-26). A mailed code gives a new password, for every
//! account, and confirms a key reset as a second factor (ADR-0073, amended
//! 2026-11-26); it never returns keys.
//!
//! # When it is on
//!
//! With `LAKKO_AUTH_EMAIL_PEPPER` (64 hex) and `LAKKO_AUTH_HANDLE_PEPPER`
//! both set. Otherwise the enrol, remove, recover and reset-factor routes
//! answer `503 {"error":"recovery-email-disabled"}` and
//! `GET /recovery-email/available` answers `{"available": false}`.
//!
//! # Enrol, replace, remove (bearer)
//!
//! - `POST /recovery-email/enrol/start {fresh_grant, handle, address, lang?}`
//!   -> `{challenge_id, resend_after_secs, expires_in_secs}`. Needs a
//!   fresh-auth grant; the handle must be the one bound to the session's
//!   account. The letter is sent before the answer, so a relay failure is
//!   `502 mail-send-failed`.
//! - `POST /recovery-email/enrol/finish {challenge_id, code}` ->
//!   `{enabled}`. Stores `email_key`.
//! - `DELETE /recovery-email {fresh_grant}` -> `{removed}`; `409
//!   {"error":"last-factor"}` when it is the account's last second factor
//!   (no confirmed authenticator, no passkey; ADR-0075, amended 2026-11-26).
//! - `GET /recovery-email` -> `{available, enabled, totp}`.
//! - Re-binding or removing the login handle deletes the key in the same
//!   transaction ([`drop_for_handle_change`]), unless the bind carries the
//!   address (`recovery_email`, ADR-0076 section 2): then the key is moved to
//!   the new handle in that transaction ([`rekey_for_handle_change`]), with
//!   no letter.
//!
//! # Recover (anonymous)
//!
//! - `POST /recovery-email/recover/start {handle, address, lang?}` always
//!   answers `{challenge_id, resend_after_secs, expires_in_secs}`. The typed
//!   pair is compared with the stored `email_key` (or a pepper-derived dummy
//!   when there is none) without an early exit; a letter is queued only on a
//!   match and sent after the answer; a miss files a decoy challenge whose
//!   code nobody was sent, so a match and a miss do the same work and answer
//!   alike.
//! - `POST /recovery-email/recover/verify {challenge_id, code}` -> `{ticket,
//!   totp_required, expires_in_secs}` or the one `401
//!   {"error":"code-refused"}` (wrong code, decoy, unknown, expired or spent
//!   challenge).
//! - `POST /recovery-email/recover/totp {ticket, code}` when required (D8).
//! - `POST /recovery-email/recover/password/{start,finish}` under the
//!   ticket, once the authenticator step (when required) passed: an OPAQUE
//!   registration for the password. `finish`, in one transaction under the
//!   account row's lock, captures the recall anchor
//!   ([`crate::recall::capture_anchor`]) BEFORE the envelope changes,
//!   replaces the envelope, deletes the in-flight password logins, writes the
//!   notice and lands a session pair. It mints **no fresh-auth grant**: a
//!   mailbox is not the password or a passkey, so whoever came in this way
//!   signs in with the new password before they can reset keys, add a device
//!   or change the email.
//!
//! # The reset factor (bearer; ADR-0073, amended 2026-11-26)
//!
//! - `POST /account/keys/reset/factor/email/start {fresh_grant, handle,
//!   address, lang?}` -> always `{challenge_id, resend_after_secs,
//!   expires_in_secs}`; a letter only when the pair matches the stored
//!   `email_key` of the bearer's account, a decoy challenge otherwise (its
//!   own purpose, `reset`, beside enrol and recover).
//! - `POST /account/keys/reset/factor/email/finish {fresh_grant,
//!   challenge_id, code}` -> `{reset_grant, reset_grant_expires_at_ms}` or
//!   `401 {"error":"factor-refused"}`.
//!
//! Both need a fresh-auth grant minted by the PASSWORD (`403
//! fresh-auth-required` otherwise, before anything is charged).
//!
//! # Codes and tickets
//!
//! A code is six digits drawn by rejection sampling from the OS generator,
//! stored only as a keyed hash bound to its challenge id, good for ten
//! minutes and five tries; one live challenge per (account, purpose). A
//! ticket is 32 random bytes, stored as a keyed hash, ten minutes, single
//! use; one live ticket per account.
//!
//! # Limits (ADR-0075 section 10; defaults chosen, not measured)
//!
//! [`Scope::EmailLetterHandle`] and the `Email` address bucket before
//! anything is looked up; [`Scope::EmailLetterAccount`] after (a spent one
//! sends no letter and, on the anonymous route, answers as always);
//! [`Scope::EmailVerify`] per login handle across challenges.
//!
//! [ADR-0075]: ../../../docs/adr/0075-recovery-email.md

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::{blake3_derive_key_from_material, blake3_keyed};
use opaque_ke::ServerRegistration;
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::{MySql, MySqlPool, Transaction};

use crate::account_notices::NoticeKind;
use crate::limits::{LimitError, Scope};
use crate::login_handle::HANDLE_LEN;
use crate::mail::{Letter, MailSender};
use crate::opaque::{
    decode_registration_request, decode_registration_upload, opaque_registration_start,
    LakkoCipherSuite, OpaqueError, KIND_PASSWORD,
};
use crate::session::{now_ms, SessionPair};
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

/// Environment variable holding the pepper: 64 hex characters (32 bytes).
pub(crate) const PEPPER_VAR: &str = "LAKKO_AUTH_EMAIL_PEPPER";

/// Domain of `email_key`. Frozen: a change orphans every stored key.
const EMAIL_KEY_DOMAIN: &[u8] = b"lakko/auth/recovery-email/v1";

/// How long a mailed code is good for.
pub(crate) const CODE_TTL_MS: i64 = 600_000;

/// Tries a code admits before its challenge is destroyed.
pub(crate) const MAX_CODE_ATTEMPTS: u32 = 5;

/// How long a ticket is good for.
pub(crate) const TICKET_TTL_MS: i64 = 600_000;

/// Authenticator-code tries a ticket admits before it is destroyed.
pub(crate) const MAX_TICKET_TOTP_ATTEMPTS: u32 = 5;

/// What the client is told to wait before offering "Send again" (UX 19.2).
/// Advisory: what bounds letters is [`Scope::EmailLetterHandle`].
pub(crate) const RESEND_AFTER_SECS: u32 = 45;

/// `email_challenges.purpose`.
const PURPOSE_ENROL: u8 = 1;
const PURPOSE_RECOVER: u8 = 2;
/// A reset factor's code (ADR-0073, amended 2026-11-26).
const PURPOSE_RESET: u8 = 3;

/// How often expired challenges and tickets are deleted.
const PRUNE_INTERVAL: Duration = Duration::from_secs(300);

/// Body codes of this module's refusals.
const CODE_DISABLED: &str = "recovery-email-disabled";
const CODE_CODE_REFUSED: &str = "code-refused";
const CODE_CODE_EXPIRED: &str = "code-expired";
const CODE_TICKET_REFUSED: &str = "ticket-refused";
const CODE_TOTP_REQUIRED: &str = "totp-required";
const CODE_TOTP_DISABLED: &str = "totp-disabled";
const CODE_ADDRESS_INVALID: &str = "address-invalid";
const CODE_HANDLE_MISMATCH: &str = "handle-mismatch";
const CODE_USERNAME_REQUIRED: &str = "username-required";
const CODE_MAIL_SEND_FAILED: &str = "mail-send-failed";

const fn disabled() -> ApiError {
    ApiError::Coded(StatusCode::SERVICE_UNAVAILABLE, CODE_DISABLED)
}

const fn code_refused() -> ApiError {
    ApiError::Coded(StatusCode::UNAUTHORIZED, CODE_CODE_REFUSED)
}

const fn ticket_refused() -> ApiError {
    ApiError::Coded(StatusCode::UNAUTHORIZED, CODE_TICKET_REFUSED)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The recovery email's keys and its sender. Present in [`AppState`] only
/// when `LAKKO_AUTH_EMAIL_PEPPER` is set.
pub(crate) struct Config {
    /// Keys `email_key`.
    pepper: [u8; 32],
    /// Keys the stored hash of a code.
    code_key: [u8; 32],
    /// Keys the stored hash of a ticket.
    ticket_key: [u8; 32],
    /// Keys the login-handle tag a challenge carries for its verify counter.
    tag_key: [u8; 32],
    /// Keys the dummy `email_key` and the stand-in account of a decoy.
    decoy_key: [u8; 32],
    /// Sends the letter.
    sender: Arc<dyn MailSender>,
}

impl Config {
    /// Derive the subkeys from the pepper.
    pub(crate) fn new(pepper: [u8; 32], sender: Arc<dyn MailSender>) -> Self {
        let sub = |domain: &str| blake3_derive_key_from_material(domain, &pepper);
        Self {
            pepper,
            code_key: sub("lakko/auth/recovery-email/code-key/v1"),
            ticket_key: sub("lakko/auth/recovery-email/ticket-key/v1"),
            tag_key: sub("lakko/auth/recovery-email/handle-tag-key/v1"),
            decoy_key: sub("lakko/auth/recovery-email/decoy-key/v1"),
            sender,
        }
    }

    /// `email_key` for a login handle and a NORMALISED address.
    fn email_key(&self, handle: &[u8; HANDLE_LEN], address: &str) -> [u8; 32] {
        // The normalised address is at most 254 bytes, so the length fits.
        let len = u16::try_from(address.len()).unwrap_or(u16::MAX);
        let mut material =
            Vec::with_capacity(EMAIL_KEY_DOMAIN.len() + HANDLE_LEN + 2 + address.len());
        material.extend_from_slice(EMAIL_KEY_DOMAIN);
        material.extend_from_slice(handle);
        material.extend_from_slice(&len.to_be_bytes());
        material.extend_from_slice(address.as_bytes());
        blake3_keyed(&self.pepper, &material)
    }

    /// What a typed pair is compared with when the account has no recovery
    /// email (or does not exist): stable per handle, equal to no real key.
    fn dummy_email_key(&self, handle: &[u8; HANDLE_LEN]) -> [u8; 32] {
        let mut material = Vec::with_capacity(6 + HANDLE_LEN);
        material.extend_from_slice(b"dummy\0");
        material.extend_from_slice(handle);
        blake3_keyed(&self.decoy_key, &material)
    }

    /// The stand-in account a decoy challenge is filed under: stable per
    /// (handle, typed pair), so repeated misses replace one row.
    fn decoy_account(&self, handle: &[u8; HANDLE_LEN], candidate: &[u8; 32]) -> [u8; 16] {
        let mut material = Vec::with_capacity(8 + HANDLE_LEN + 32);
        material.extend_from_slice(b"account\0");
        material.extend_from_slice(handle);
        material.extend_from_slice(candidate);
        let digest = blake3_keyed(&self.decoy_key, &material);
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest[..16]);
        out
    }

    /// The stored hash of `code` for the challenge `id`.
    fn hash_code(&self, id: &[u8; 16], code: &str) -> [u8; 32] {
        let mut material = Vec::with_capacity(16 + code.len());
        material.extend_from_slice(id);
        material.extend_from_slice(code.as_bytes());
        blake3_keyed(&self.code_key, &material)
    }

    fn hash_ticket(&self, ticket: &[u8; 32]) -> [u8; 32] {
        blake3_keyed(&self.ticket_key, ticket)
    }

    fn handle_tag(&self, handle: &[u8; HANDLE_LEN]) -> [u8; 32] {
        blake3_keyed(&self.tag_key, handle)
    }
}

/// Build the configuration from the environment: `Ok(None)` when the pepper
/// is unset.
///
/// # Errors
///
/// A pepper that is not 64 hex characters, or a malformed
/// `LAKKO_AUTH_SMTP_*` value.
pub(crate) fn load() -> anyhow::Result<Option<Config>> {
    let raw = match std::env::var(PEPPER_VAR) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(None),
    };
    let pepper = crate::login_handle::parse_hex32(raw.trim())
        .ok_or_else(|| anyhow::anyhow!("{PEPPER_VAR} must be 64 hex characters (32 bytes)"))?;
    Ok(Some(Config::new(pepper, crate::mail::sender_from_env()?)))
}

/// Log the startup state, loudly when dormant or when letters go nowhere.
pub(crate) fn log_startup(state: &AppState) {
    match state.recovery_email.as_ref() {
        None => tracing::warn!(
            "recovery email DISABLED: {PEPPER_VAR} is not set; its routes answer 503"
        ),
        Some(_) if state.handle_pepper.is_none() => {
            tracing::warn!(
                "recovery email DISABLED: it also needs LAKKO_AUTH_HANDLE_PEPPER; its routes \
                 answer 503"
            );
        },
        Some(config) => {
            let sender = config.sender.name();
            if sender == "log-only" {
                tracing::warn!(
                    "recovery email enabled with the LOG-ONLY sender (LAKKO_AUTH_SMTP_HOST \
                     unset): no letter is delivered, so no code can be received"
                );
            } else {
                tracing::info!(sender, "recovery email enabled");
            }
        },
    }
}

/// Everything a route of this module needs to run.
struct Enabled {
    config: Arc<Config>,
    handle_pepper: [u8; 32],
}

/// The configuration, or the `503` every gated route answers without it.
fn enabled(state: &AppState) -> Result<Enabled, ApiError> {
    match (state.recovery_email.as_ref(), state.handle_pepper.as_ref()) {
        (Some(config), Some(handle_pepper)) => Ok(Enabled {
            config: config.clone(),
            handle_pepper: **handle_pepper,
        }),
        _ => Err(disabled()),
    }
}

/// Whether the recovery email is on for this server.
fn is_available(state: &AppState) -> bool {
    enabled(state).is_ok()
}

// ---------------------------------------------------------------------------
// Small pure pieces
// ---------------------------------------------------------------------------

/// Normalise a recovery email address with the ONE shared rule
/// (`lakko_mnemonic::recovery_email::normalize_recovery_email`: trim,
/// lower-case the whole address, no control character and no white space, 3
/// to 254 bytes, exactly one `@` with text on both sides, no provider
/// rules). The client builds the address it sends from the same function,
/// so the two cannot disagree on a byte. `None` when the address is refused.
fn normalise_address(raw: &str) -> Option<String> {
    lakko_mnemonic::recovery_email::normalize_recovery_email(raw).ok()
}

/// Six decimal digits, uniform: 32 random bits are drawn until they fall
/// below the largest multiple of one million that fits, then reduced.
fn generate_code() -> String {
    const MODULUS: u32 = 1_000_000;
    // 4294 * 1_000_000: values at or above it would bias the low codes.
    const LIMIT: u32 = (u32::MAX / MODULUS) * MODULUS;
    loop {
        let draw = OsRng.next_u32();
        if draw < LIMIT {
            return format!("{:06}", draw % MODULUS);
        }
    }
}

/// Compare two 32-byte values without an early exit.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    OsRng.fill_bytes(&mut out);
    out
}

/// The device's language tag, kept only when it looks like one. Never
/// logged; it chooses the letter's template.
fn clean_language(lang: Option<String>) -> Option<String> {
    lang.filter(|l| {
        !l.is_empty() && l.len() <= 35 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Decode a wire `challenge_id`: URL-safe base64 without padding, 16 bytes.
fn decode_challenge_id(b64: &str) -> Option<[u8; 16]> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes.as_slice()).ok())
}

/// A typed code as the hash takes it: trimmed, and empty when it is too long
/// to be one (so an oversized body costs no hashing of it).
fn typed_code(code: &str) -> &str {
    let trimmed = code.trim();
    if trimmed.len() > 16 {
        ""
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// Challenges
// ---------------------------------------------------------------------------

/// A challenge to store.
struct NewChallenge<'a> {
    id: [u8; 16],
    account_id: &'a [u8; 16],
    purpose: u8,
    code_key: [u8; 32],
    pending_key: Option<[u8; 32]>,
    handle_tag: [u8; 32],
}

/// Store `challenge`, replacing the live one of its (account, purpose). One
/// statement, so no gap lock is held across a second one.
async fn put_challenge(
    pool: &MySqlPool,
    challenge: &NewChallenge<'_>,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_challenges \
            (id, account_id, purpose, code_key, pending_key, handle_tag, attempts, \
             expires_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?) \
         ON DUPLICATE KEY UPDATE \
            id = VALUES(id), code_key = VALUES(code_key), pending_key = VALUES(pending_key), \
            handle_tag = VALUES(handle_tag), attempts = 0, \
            expires_at_ms = VALUES(expires_at_ms)",
    )
    .bind(&challenge.id[..])
    .bind(&challenge.account_id[..])
    .bind(challenge.purpose)
    .bind(&challenge.code_key[..])
    .bind(challenge.pending_key.as_ref().map(|k| &k[..]))
    .bind(&challenge.handle_tag[..])
    .bind(now.saturating_add(CODE_TTL_MS))
    .execute(pool)
    .await
    .map(|_| ())
}

/// A live challenge as `verify` reads it.
struct Challenge {
    account_id: [u8; 16],
    code_key: [u8; 32],
    pending_key: Option<[u8; 32]>,
    handle_tag: [u8; 32],
}

type ChallengeRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, Vec<u8>);

/// The unexpired challenge `id` of `purpose`, if any.
async fn load_challenge(
    pool: &MySqlPool,
    id: &[u8; 16],
    purpose: u8,
    now: i64,
) -> Result<Option<Challenge>, sqlx::Error> {
    let row: Option<ChallengeRow> = sqlx::query_as(
        "SELECT account_id, code_key, pending_key, handle_tag \
         FROM email_challenges WHERE id = ? AND purpose = ? AND expires_at_ms > ?",
    )
    .bind(&id[..])
    .bind(purpose)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(account, code_key, pending, tag)| {
        Some(Challenge {
            account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
            code_key: <[u8; 32]>::try_from(code_key.as_slice()).ok()?,
            pending_key: pending.and_then(|p| <[u8; 32]>::try_from(p.as_slice()).ok()),
            handle_tag: <[u8; 32]>::try_from(tag.as_slice()).ok()?,
        })
    }))
}

/// How checking a code against a challenge ended.
enum CodeOutcome {
    /// The code is the challenge's; the challenge is consumed.
    Accepted(Challenge),
    /// A live challenge and another code; one try is spent (the fifth
    /// destroys the challenge).
    Wrong,
    /// No such live challenge: unknown, expired, of another purpose, spent.
    Gone,
}

/// Check `code` against the loaded `challenge` of id `id`: count the try
/// first (atomically, so five concurrent guesses are five tries), compare
/// without an early exit, and consume the challenge on a match (of two
/// matching requests one deletes it).
async fn check_code(
    pool: &MySqlPool,
    config: &Config,
    id: &[u8; 16],
    challenge: Challenge,
    code: &str,
    now: i64,
) -> Result<CodeOutcome, sqlx::Error> {
    let counted = sqlx::query(
        "UPDATE email_challenges SET attempts = attempts + 1 \
         WHERE id = ? AND attempts < ? AND expires_at_ms > ?",
    )
    .bind(&id[..])
    .bind(MAX_CODE_ATTEMPTS)
    .bind(now)
    .execute(pool)
    .await?
    .rows_affected();
    if counted != 1 {
        sqlx::query("DELETE FROM email_challenges WHERE id = ?")
            .bind(&id[..])
            .execute(pool)
            .await?;
        return Ok(CodeOutcome::Gone);
    }
    if ct_eq(&config.hash_code(id, typed_code(code)), &challenge.code_key) {
        let consumed = sqlx::query("DELETE FROM email_challenges WHERE id = ?")
            .bind(&id[..])
            .execute(pool)
            .await?
            .rows_affected();
        return Ok(if consumed == 1 {
            CodeOutcome::Accepted(challenge)
        } else {
            CodeOutcome::Gone
        });
    }
    // The try that spent the budget takes the challenge with it.
    sqlx::query("DELETE FROM email_challenges WHERE id = ? AND attempts >= ?")
        .bind(&id[..])
        .bind(MAX_CODE_ATTEMPTS)
        .execute(pool)
        .await?;
    Ok(CodeOutcome::Wrong)
}

// ---------------------------------------------------------------------------
// GET /recovery-email/available, GET /recovery-email
// ---------------------------------------------------------------------------

/// `GET /recovery-email/available` — anonymous. Whether this server can send
/// a recovery code at all; a client hides the email rows when it cannot.
#[axum::debug_handler]
pub(crate) async fn available(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "available": is_available(&state) }))
}

/// Answer of `GET /recovery-email`.
#[derive(Serialize)]
pub(crate) struct StatusRes {
    /// Whether this server can send a recovery code.
    pub(crate) available: bool,
    /// The account has a recovery email. Never the address.
    pub(crate) enabled: bool,
    /// The account has a confirmed authenticator: a mailed code will not be
    /// enough without its code (D8), which the enrolment screens say.
    pub(crate) totp: bool,
}

/// `GET /recovery-email` — bearer. The session's account only. Not gated on
/// the pepper: a server that sends no mail answers `available: false`.
///
/// # Errors
///
/// [`ApiError::Db`]; [`ApiError::InvalidCredential`] when the account is gone.
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<StatusRes>, ApiError> {
    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT (SELECT 1 FROM recovery_emails e WHERE e.account_id = a.id), \
                (SELECT 1 FROM totp_secrets t \
                  WHERE t.account_id = a.id AND t.confirmed_at IS NOT NULL) \
         FROM accounts a WHERE a.id = ?",
    )
    .bind(&auth.0[..])
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let (email, totp) = row.ok_or(ApiError::InvalidCredential)?;
    Ok(Json(StatusRes {
        available: is_available(&state),
        enabled: email.is_some(),
        totp: totp.is_some(),
    }))
}

// ---------------------------------------------------------------------------
// Enrol
// ---------------------------------------------------------------------------

/// Require that `handle` is the login handle bound to `account_id`: `409
/// username-required` when the account has none, `409 handle-mismatch` when
/// it has another.
async fn require_own_handle(
    state: &AppState,
    on: &Enabled,
    account_id: &[u8; 16],
    handle: &[u8; HANDLE_LEN],
) -> Result<(), ApiError> {
    let resolved = crate::login_handle::resolve(&state.db, &on.handle_pepper, handle).await?;
    if resolved.bound && &resolved.account_id == account_id {
        return Ok(());
    }
    let has_handle: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM login_handles WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;
    Err(ApiError::Coded(
        StatusCode::CONFLICT,
        if has_handle.is_some() {
            CODE_HANDLE_MISMATCH
        } else {
            CODE_USERNAME_REQUIRED
        },
    ))
}

/// Body of `POST /recovery-email/enrol/start`.
#[derive(Deserialize)]
pub(crate) struct EnrolStartReq {
    /// The `k1` fresh-auth grant. Optional on the wire so that its absence
    /// is the `403 fresh-auth-required` every other refusal gets.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// The account's login handle: 32 bytes, base64url without padding.
    pub(crate) handle: String,
    /// The address as the person typed it.
    pub(crate) address: String,
    /// The device's language tag, for the letter. Optional.
    #[serde(default)]
    pub(crate) lang: Option<String>,
}

/// Answer of `POST /recovery-email/enrol/start`.
#[derive(Serialize)]
pub(crate) struct EnrolStartRes {
    /// URL-safe base64 of the 16-byte challenge id, echoed in `finish`.
    pub(crate) challenge_id: String,
    /// Seconds before the client offers "Send again".
    pub(crate) resend_after_secs: u32,
    /// Seconds the code is good for.
    pub(crate) expires_in_secs: u32,
}

/// `POST /recovery-email/enrol/start` — bearer + fresh-auth grant.
///
/// Order: the feature (`503`), the grant (`403 fresh-auth-required`), the
/// body (`400`, with `address-invalid` for an address that is refused), the
/// handle is the session account's bound one (`409 username-required` when
/// the account has none, `409 handle-mismatch` otherwise), the letter
/// counters (`429 email-rate-limited`), the challenge, the letter (`502
/// mail-send-failed`; the challenge is withdrawn).
///
/// # Errors
///
/// As listed; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn enrol_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrolStartReq>,
) -> Result<Json<EnrolStartRes>, ApiError> {
    let on = enabled(&state)?;
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let address = normalise_address(&req.address).ok_or(ApiError::Coded(
        StatusCode::BAD_REQUEST,
        CODE_ADDRESS_INVALID,
    ))?;

    require_own_handle(&state, &on, &account_id, &handle).await?;

    crate::limits::charge_name(&state.db, &state.limits, Scope::EmailLetterHandle, &handle).await?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailLetterAccount,
        &account_id,
    )
    .await?;

    let id: [u8; 16] = random();
    let code = generate_code();
    let now = now_ms();
    put_challenge(
        &state.db,
        &NewChallenge {
            id,
            account_id: &account_id,
            purpose: PURPOSE_ENROL,
            code_key: on.config.hash_code(&id, &code),
            pending_key: Some(on.config.email_key(&handle, &address)),
            handle_tag: on.config.handle_tag(&handle),
        },
        now,
    )
    .await
    .map_err(ApiError::Db)?;

    let letter = Letter {
        to: address,
        code,
        language: clean_language(req.lang),
    };
    if let Err(e) = on.config.sender.send(letter).await {
        tracing::warn!(error = %e, "recovery email: the enrol letter was not sent");
        sqlx::query("DELETE FROM email_challenges WHERE id = ?")
            .bind(&id[..])
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?;
        return Err(ApiError::Coded(
            StatusCode::BAD_GATEWAY,
            CODE_MAIL_SEND_FAILED,
        ));
    }
    tracing::info!("recovery email: enrol code sent");
    Ok(Json(EnrolStartRes {
        challenge_id: URL_SAFE_NO_PAD.encode(id),
        resend_after_secs: RESEND_AFTER_SECS,
        expires_in_secs: ttl_secs(CODE_TTL_MS),
    }))
}

fn ttl_secs(ms: i64) -> u32 {
    u32::try_from(ms / 1000).unwrap_or(u32::MAX)
}

/// Body of `POST /recovery-email/enrol/finish`.
#[derive(Deserialize)]
pub(crate) struct EnrolFinishReq {
    /// The `challenge_id` the start returned.
    pub(crate) challenge_id: String,
    /// The six digits from the letter.
    pub(crate) code: String,
}

/// Answer of `POST /recovery-email/enrol/finish`.
#[derive(Serialize)]
pub(crate) struct EnrolFinishRes {
    /// Always `true`: the account has a recovery email.
    pub(crate) enabled: bool,
}

/// `POST /recovery-email/enrol/finish` — bearer.
///
/// A wrong code is `401 code-refused` (five tries); a challenge that is not
/// there (expired, spent, another account's, replaced by a newer start) is
/// `410 code-expired`. On the right code, one transaction under the account
/// row's lock: the `email_key` is stored (replacing the previous address's),
/// the account's recovery tickets are deleted, and the "added" / "replaced"
/// notice written.
///
/// # Errors
///
/// As above; [`ApiError::BadInput`]; [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn enrol_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<EnrolFinishReq>,
) -> Result<Json<EnrolFinishRes>, ApiError> {
    let on = enabled(&state)?;
    let account_id = auth.0;
    let id = decode_challenge_id(&req.challenge_id).ok_or(ApiError::BadInput(
        "challenge_id must be 16 bytes of URL-safe base64",
    ))?;

    let gone = || ApiError::Coded(StatusCode::GONE, CODE_CODE_EXPIRED);
    let now = now_ms();
    let challenge = load_challenge(&state.db, &id, PURPOSE_ENROL, now)
        .await
        .map_err(ApiError::Db)?
        .filter(|c| c.account_id == account_id)
        .ok_or_else(gone)?;
    let challenge = match check_code(&state.db, &on.config, &id, challenge, &req.code, now)
        .await
        .map_err(ApiError::Db)?
    {
        CodeOutcome::Accepted(challenge) => challenge,
        CodeOutcome::Wrong => return Err(code_refused()),
        CodeOutcome::Gone => return Err(gone()),
    };
    let Some(email_key) = challenge.pending_key else {
        tracing::error!("recovery email: an enrol challenge without its pending key");
        return Err(ApiError::Internal);
    };

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    lock_account(&mut tx, &account_id).await?;
    // Under the row lock nothing else writes this account's row, so the read
    // and the upsert below see the same state.
    let had: Option<i64> = sqlx::query_scalar("SELECT 1 FROM recovery_emails WHERE account_id = ?")
        .bind(&account_id[..])
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    sqlx::query(
        "INSERT INTO recovery_emails (account_id, email_key) VALUES (?, ?) \
         ON DUPLICATE KEY UPDATE email_key = VALUES(email_key), created_at = NOW(3)",
    )
    .bind(&account_id[..])
    .bind(&email_key[..])
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    // A ticket earned with the previous address must not outlive it.
    sqlx::query("DELETE FROM email_tickets WHERE account_id = ?")
        .bind(&account_id[..])
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    let kind = if had.is_some() {
        NoticeKind::EmailReplaced
    } else {
        NoticeKind::EmailAdded
    };
    crate::account_notices::write(&mut tx, &account_id, kind, now)
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(replaced = had.is_some(), "recovery email: enrolled");
    Ok(Json(EnrolFinishRes { enabled: true }))
}

/// Body of `DELETE /recovery-email`.
#[derive(Deserialize)]
pub(crate) struct GrantReq {
    /// The `k1` fresh-auth grant.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
}

/// Delete everything the recovery email keeps for `account_id`: the key, the
/// tickets and the challenges. Returns whether there was a key.
async fn delete_rows(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
) -> Result<bool, sqlx::Error> {
    let had = sqlx::query("DELETE FROM recovery_emails WHERE account_id = ?")
        .bind(&account_id[..])
        .execute(&mut **tx)
        .await?
        .rows_affected()
        > 0;
    for statement in [
        "DELETE FROM email_tickets WHERE account_id = ?",
        "DELETE FROM email_challenges WHERE account_id = ?",
    ] {
        sqlx::query(statement)
            .bind(&account_id[..])
            .execute(&mut **tx)
            .await?;
    }
    Ok(had)
}

/// The login handle of `account_id` is changing or going away: the stored
/// `email_key` was made from the old name, so the recovery email is turned off in the SAME transaction
/// (ADR-0075 section 6) and the account's devices are told.
///
/// # Errors
///
/// Any `sqlx::Error`; the caller rolls the handle change back with it.
pub(crate) async fn drop_for_handle_change(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
) -> Result<(), sqlx::Error> {
    if delete_rows(tx, account_id).await? {
        crate::account_notices::write(tx, account_id, NoticeKind::EmailRemoved, now_ms()).await?;
    }
    Ok(())
}

/// The `recovery_email` object of `PUT /auth/login-handle` (ADR-0076 section
/// 2): the address the account confirmed, so a change of handle re-keys the
/// recovery email instead of dropping it.
#[derive(Deserialize)]
pub(crate) struct RekeyReq {
    /// The address, as the device kept it.
    pub(crate) address: String,
}

/// What a handle change does to the recovery email, computed before the
/// bind's transaction: everything but the comparison with the stored row.
pub(crate) struct RekeyPlan {
    /// `email_key` of the address under the handle the client says the
    /// account holds now (`previous_handle`); `None` when it sent none.
    old_email_key: Option<[u8; 32]>,
    /// `email_key` of the address under the NEW handle.
    new_email_key: [u8; 32],
}

/// How a re-key inside the bind's transaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RekeyOutcome {
    /// The account has no recovery email: nothing to do.
    Nothing,
    /// The key now names the new handle.
    Rekeyed,
    /// The address under the previous handle is not the stored key's. The
    /// caller rolls the whole bind back.
    AddressMismatch,
}

/// Validate a [`RekeyReq`] and compute its [`RekeyPlan`].
///
/// # Errors
///
/// `503 recovery-email-disabled` when the feature is off (the server could
/// not check the address, and dropping the email against the client's stated
/// wish would be worse than refusing); `400 address-invalid`.
pub(crate) fn rekey_plan(
    state: &AppState,
    previous_handle: Option<&[u8; HANDLE_LEN]>,
    new_handle: &[u8; HANDLE_LEN],
    req: &RekeyReq,
) -> Result<RekeyPlan, ApiError> {
    let on = enabled(state)?;
    let address = normalise_address(&req.address).ok_or(ApiError::Coded(
        StatusCode::BAD_REQUEST,
        CODE_ADDRESS_INVALID,
    ))?;
    Ok(RekeyPlan {
        old_email_key: previous_handle.map(|old| on.config.email_key(old, &address)),
        new_email_key: on.config.email_key(new_handle, &address),
    })
}

/// Move the account's recovery email to its new handle inside the bind's
/// transaction (the caller holds the account row's lock and has checked that
/// `previous_handle` is the handle the account holds).
///
/// With no `recovery_emails` row there is nothing to move. Otherwise the
/// address under the previous handle must be the stored key's (compared
/// without an early exit); then the key is rewritten under the new handle
/// and the account's challenges and tickets are dropped. No notice: the
/// recovery email is the same one.
///
/// # Errors
///
/// Any `sqlx::Error`; the caller rolls the bind back.
pub(crate) async fn rekey_for_handle_change(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    plan: &RekeyPlan,
) -> Result<RekeyOutcome, sqlx::Error> {
    let stored: Option<[u8; 32]> = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT email_key FROM recovery_emails WHERE account_id = ?",
    )
    .bind(&account_id[..])
    .fetch_optional(&mut **tx)
    .await?
    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
    let Some(stored) = stored else {
        // Whatever else the account had under the old name goes, as without
        // a re-key.
        delete_rows(tx, account_id).await?;
        return Ok(RekeyOutcome::Nothing);
    };
    let matches = plan
        .old_email_key
        .as_ref()
        .is_some_and(|old| ct_eq(old, &stored));
    if !matches {
        return Ok(RekeyOutcome::AddressMismatch);
    }
    sqlx::query("UPDATE recovery_emails SET email_key = ? WHERE account_id = ?")
        .bind(&plan.new_email_key[..])
        .bind(&account_id[..])
        .execute(&mut **tx)
        .await?;
    for statement in [
        "DELETE FROM email_tickets WHERE account_id = ?",
        "DELETE FROM email_challenges WHERE account_id = ?",
    ] {
        sqlx::query(statement)
            .bind(&account_id[..])
            .execute(&mut **tx)
            .await?;
    }
    Ok(RekeyOutcome::Rekeyed)
}

/// `DELETE /recovery-email {fresh_grant}` — bearer + fresh-auth grant.
/// Removes the key. `removed` says whether there was one. Refused `409
/// {"error":"last-factor"}` when the account has a recovery email and no
/// confirmed authenticator and no passkey (ADR-0075, amended 2026-11-26),
/// checked under the account row's lock.
///
/// # Errors
///
/// `503 recovery-email-disabled`, [`ApiError::FreshAuthRequired`], `409
/// last-factor`, [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn remove(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<GrantReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    enabled(&state)?;
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;
    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    lock_account(&mut tx, &account_id).await?;
    crate::factors::refuse_if_last(&mut tx, &account_id, crate::factors::Leaving::RecoveryEmail)
        .await?;
    let removed = delete_rows(&mut tx, &account_id)
        .await
        .map_err(ApiError::Db)?;
    if removed {
        crate::account_notices::write(&mut tx, &account_id, NoticeKind::EmailRemoved, now_ms())
            .await
            .map_err(ApiError::Db)?;
    }
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!(removed, "recovery email: removed");
    Ok(Json(serde_json::json!({ "removed": removed })))
}

/// Take the account row's lock; `401` when the account is gone.
async fn lock_account(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
) -> Result<u64, ApiError> {
    sqlx::query_scalar::<_, u64>("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
        .bind(&account_id[..])
        .fetch_optional(&mut **tx)
        .await
        .map_err(ApiError::Db)?
        .ok_or(ApiError::InvalidCredential)
}

// ---------------------------------------------------------------------------
// Recover: start, verify
// ---------------------------------------------------------------------------

/// Body of `POST /recovery-email/recover/start`.
#[derive(Deserialize)]
pub(crate) struct RecoverStartReq {
    /// The login handle of the account: 32 bytes, base64url without padding.
    pub(crate) handle: String,
    /// The address as the person typed it.
    pub(crate) address: String,
    /// The device's language tag, for the letter. Optional.
    #[serde(default)]
    pub(crate) lang: Option<String>,
}

/// Answer of `POST /recovery-email/recover/start`: the same for a matching
/// pair, a wrong pair, an unknown name and an account without an email.
#[derive(Serialize)]
pub(crate) struct RecoverStartRes {
    /// URL-safe base64 of the 16-byte challenge id.
    pub(crate) challenge_id: String,
    /// Seconds before the client offers "Send again".
    pub(crate) resend_after_secs: u32,
    /// Seconds a code is good for.
    pub(crate) expires_in_secs: u32,
}

/// `POST /recovery-email/recover/start` — anonymous, per-address limited.
///
/// Order: the feature (`503`), the body (`400`; an address that is refused
/// is `400 address-invalid`, which says nothing about any account), the
/// per-handle letter counter (`429`, for a known and an unknown name alike),
/// then the lookup. Every path past the counter does the same work: one
/// handle resolution, one `recovery_emails` read, one comparison without an
/// early exit (against a pepper-derived dummy when there is no key), one
/// per-account counter bump (a decoy account's for a miss), one challenge
/// upsert. Only what the challenge holds differs: the hash of a code that is
/// mailed, or random bytes no code matches. The letter is sent after the
/// answer, on its own task.
///
/// # Errors
///
/// As listed; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn recover_start(
    State(state): State<AppState>,
    Json(req): Json<RecoverStartReq>,
) -> Result<Json<RecoverStartRes>, ApiError> {
    let on = enabled(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let address = normalise_address(&req.address).ok_or(ApiError::Coded(
        StatusCode::BAD_REQUEST,
        CODE_ADDRESS_INVALID,
    ))?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::EmailLetterHandle, &handle).await?;

    let resolved = crate::login_handle::resolve(&state.db, &on.handle_pepper, &handle).await?;
    let stored: Option<[u8; 32]> = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT email_key FROM recovery_emails WHERE account_id = ?",
    )
    .bind(&resolved.account_id[..])
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?
    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
    let candidate = on.config.email_key(&handle, &address);
    let reference = stored.unwrap_or_else(|| on.config.dummy_email_key(&handle));
    // Both operands are evaluated; `&` not `&&`.
    let matched = ct_eq(&candidate, &reference) & stored.is_some();

    let decoy = on.config.decoy_account(&handle, &candidate);
    let counted_account = if matched { resolved.account_id } else { decoy };
    // A spent or failing per-account counter sends no letter and changes
    // nothing a caller can see.
    let admitted = match crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailLetterAccount,
        &counted_account,
    )
    .await
    {
        Ok(()) => true,
        Err(LimitError::Limited { .. }) => false,
        Err(LimitError::Unavailable) => {
            tracing::error!("recovery email: the per-account letter counter failed; no letter");
            false
        },
    };
    let send = matched & admitted;

    let id: [u8; 16] = random();
    let code = generate_code();
    let real_code_key = on.config.hash_code(&id, &code);
    let now = now_ms();
    // A real challenge replaces the account's live one only when a letter
    // carries its code; otherwise the row is a decoy and the account's live
    // challenge, if it has one, is left alone.
    let filed_under = if send { resolved.account_id } else { decoy };
    put_challenge(
        &state.db,
        &NewChallenge {
            id,
            account_id: &filed_under,
            purpose: PURPOSE_RECOVER,
            code_key: if send { real_code_key } else { random() },
            pending_key: None,
            handle_tag: on.config.handle_tag(&handle),
        },
        now,
    )
    .await
    .map_err(ApiError::Db)?;

    if send {
        let config = on.config.clone();
        let letter = Letter {
            to: address,
            code,
            language: clean_language(req.lang),
        };
        // After the answer, so the relay's latency is not the caller's.
        tokio::spawn(async move {
            if let Err(e) = config.sender.send(letter).await {
                tracing::warn!(error = %e, "recovery email: a recover letter was not sent");
            }
        });
    }
    Ok(Json(RecoverStartRes {
        challenge_id: URL_SAFE_NO_PAD.encode(id),
        resend_after_secs: RESEND_AFTER_SECS,
        expires_in_secs: ttl_secs(CODE_TTL_MS),
    }))
}

/// Body of `POST /recovery-email/recover/verify`.
#[derive(Deserialize)]
pub(crate) struct RecoverVerifyReq {
    /// The `challenge_id` the start returned.
    pub(crate) challenge_id: String,
    /// The six digits from the letter.
    pub(crate) code: String,
}

/// Answer of `POST /recovery-email/recover/verify`.
#[derive(Serialize)]
pub(crate) struct RecoverVerifyRes {
    /// URL-safe base64 of the 32-byte ticket: single use, ten minutes.
    pub(crate) ticket: String,
    /// The account has an authenticator: `recover/totp` must pass before
    /// the password routes answer.
    pub(crate) totp_required: bool,
    /// Seconds the ticket is good for.
    pub(crate) expires_in_secs: u32,
}

/// `POST /recovery-email/recover/verify` — anonymous, per-address limited.
///
/// Everything that is not the right code for a live, real challenge is one
/// answer, `401 {"error":"code-refused"}`: a wrong code, a decoy (its stored
/// hash matches no code), an unknown, expired or spent challenge, a malformed
/// id. A live challenge (real or decoy) charges [`Scope::EmailVerify`] for
/// the login handle it was opened for before the try is counted (`429
/// too-many-attempts`). On the right code the challenge is consumed and a
/// ticket minted, replacing any ticket the account had.
///
/// # Errors
///
/// As above; `503 recovery-email-disabled`; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn recover_verify(
    State(state): State<AppState>,
    Json(req): Json<RecoverVerifyReq>,
) -> Result<Json<RecoverVerifyRes>, ApiError> {
    let on = enabled(&state)?;
    let now = now_ms();
    let id = decode_challenge_id(&req.challenge_id);
    let challenge = match id {
        Some(id) => load_challenge(&state.db, &id, PURPOSE_RECOVER, now)
            .await
            .map_err(ApiError::Db)?
            .map(|c| (id, c)),
        None => None,
    };
    let Some((id, challenge)) = challenge else {
        // The same hash work as a live challenge's comparison.
        let _ = ct_eq(
            &on.config.hash_code(&[0u8; 16], typed_code(&req.code)),
            &[0u8; 32],
        );
        return Err(code_refused());
    };
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailVerify,
        &challenge.handle_tag,
    )
    .await?;
    let CodeOutcome::Accepted(challenge) =
        check_code(&state.db, &on.config, &id, challenge, &req.code, now)
            .await
            .map_err(ApiError::Db)?
    else {
        return Err(code_refused());
    };

    let account_id = challenge.account_id;
    let totp_required = crate::totp::is_enabled(&state.db, &account_id)
        .await
        .map_err(ApiError::Db)?;
    let ticket: [u8; 32] = random();
    // One live ticket per account. Two statements, not one transaction: a
    // delete of rows that may not exist followed by an insert is the gap-lock
    // shape that deadlocks neighbouring accounts.
    sqlx::query("DELETE FROM email_tickets WHERE account_id = ?")
        .bind(&account_id[..])
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let inserted = sqlx::query(
        "INSERT INTO email_tickets (ticket_key, account_id, totp_required, expires_at_ms) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&on.config.hash_ticket(&ticket)[..])
    .bind(&account_id[..])
    .bind(totp_required)
    .bind(now.saturating_add(TICKET_TTL_MS))
    .execute(&state.db)
    .await;
    if let Err(e) = inserted {
        // The account was deleted between the code and here (the foreign
        // key): nothing to recover.
        tracing::debug!(error = %e, "recovery email: ticket not stored");
        return Err(code_refused());
    }
    tracing::info!(totp_required, "recovery email: code accepted");
    Ok(Json(RecoverVerifyRes {
        ticket: URL_SAFE_NO_PAD.encode(ticket),
        totp_required,
        expires_in_secs: ttl_secs(TICKET_TTL_MS),
    }))
}

// ---------------------------------------------------------------------------
// Recover: tickets, totp
// ---------------------------------------------------------------------------

/// A live ticket as the routes read it.
struct Ticket {
    key: [u8; 32],
    account_id: [u8; 16],
    totp_required: bool,
    totp_passed: bool,
}

impl Ticket {
    /// Whether the authenticator step is behind this ticket (D8).
    const fn cleared(&self) -> bool {
        !self.totp_required || self.totp_passed
    }
}

/// The unexpired ticket a request carries, or `401 ticket-refused`: one
/// answer for a malformed, unknown, expired and spent ticket.
async fn load_ticket(
    state: &AppState,
    config: &Config,
    ticket_b64: &str,
) -> Result<Ticket, ApiError> {
    let ticket: [u8; 32] = URL_SAFE_NO_PAD
        .decode(ticket_b64)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(ticket_refused)?;
    let key = config.hash_ticket(&ticket);
    let row: Option<(Vec<u8>, bool, bool)> = sqlx::query_as(
        "SELECT account_id, totp_required, totp_passed \
         FROM email_tickets WHERE ticket_key = ? AND expires_at_ms > ?",
    )
    .bind(&key[..])
    .bind(now_ms())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    row.and_then(|(account, totp_required, totp_passed)| {
        Some(Ticket {
            key,
            account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
            totp_required,
            totp_passed,
        })
    })
    .ok_or_else(ticket_refused)
}

/// Body of `POST /recovery-email/recover/totp`.
#[derive(Deserialize)]
pub(crate) struct RecoverTotpReq {
    /// The ticket `verify` returned.
    pub(crate) ticket: String,
    /// The six digits the authenticator shows.
    pub(crate) code: String,
}

/// `POST /recovery-email/recover/totp` — anonymous, per-address limited. The
/// authenticator step of a ticket that requires it (ADR-0075 D8), with the
/// rules of `/totp/recover`: the +-1 step window, strictly newer than the
/// last accepted step, one acceptance per step.
///
/// A ticket that requires nothing, or already passed, answers `{"ok":true}`.
/// A wrong code is `401 code-refused`; the fifth destroys the ticket. Each
/// try is charged to [`Scope::TotpRecover`] for the account.
///
/// # Errors
///
/// `401 ticket-refused`, `401 code-refused`, `429 too-many-attempts`, `503
/// totp-disabled` when `LAKKO_TOTP_ENC_KEY` is unset; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn recover_totp(
    State(state): State<AppState>,
    Json(req): Json<RecoverTotpReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let on = enabled(&state)?;
    let ticket = load_ticket(&state, &on.config, &req.ticket).await?;
    if ticket.cleared() {
        return Ok(Json(serde_json::json!({ "ok": true })));
    }
    if state.totp_enc_key.is_none() {
        return Err(ApiError::Coded(
            StatusCode::SERVICE_UNAVAILABLE,
            CODE_TOTP_DISABLED,
        ));
    }
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::TotpRecover,
        &ticket.account_id,
    )
    .await?;
    let counted = sqlx::query(
        "UPDATE email_tickets SET totp_attempts = totp_attempts + 1 \
         WHERE ticket_key = ? AND totp_attempts < ?",
    )
    .bind(&ticket.key[..])
    .bind(MAX_TICKET_TOTP_ATTEMPTS)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if counted != 1 {
        sqlx::query("DELETE FROM email_tickets WHERE ticket_key = ?")
            .bind(&ticket.key[..])
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?;
        return Err(ticket_refused());
    }
    if !crate::totp::check_code(&state, &ticket.account_id, &req.code).await? {
        return Err(code_refused());
    }
    sqlx::query("UPDATE email_tickets SET totp_passed = 1 WHERE ticket_key = ?")
        .bind(&ticket.key[..])
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Recover: the password
// ---------------------------------------------------------------------------

/// The gate of both password routes: the authenticator step, when the
/// ticket requires it (`403 totp-required`).
fn require_cleared(ticket: &Ticket) -> Result<(), ApiError> {
    if ticket.cleared() {
        Ok(())
    } else {
        Err(ApiError::Coded(StatusCode::FORBIDDEN, CODE_TOTP_REQUIRED))
    }
}

/// Body of `POST /recovery-email/recover/password/start`.
#[derive(Deserialize)]
pub(crate) struct PasswordStartReq {
    /// The ticket `verify` returned.
    pub(crate) ticket: String,
    /// Standard base64 of the OPAQUE `RegistrationRequest` for the new
    /// password.
    pub(crate) new_password_request: String,
}

/// `POST /recovery-email/recover/password/start` — anonymous, per-address
/// limited. The OPAQUE registration start for the password, under the
/// ticket. Stateless; the ticket is not spent.
///
/// # Errors
///
/// `401 ticket-refused`, `403 totp-required`, `400` for a request that does
/// not decode; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn password_start(
    State(state): State<AppState>,
    Json(req): Json<PasswordStartReq>,
) -> Result<Json<serde_json::Value>, OpaqueError> {
    let on = enabled(&state)?;
    let request = decode_registration_request(&req.new_password_request)?;
    let ticket = load_ticket(&state, &on.config, &req.ticket).await?;
    require_cleared(&ticket)?;
    let response = opaque_registration_start(
        &state.opaque_setup,
        &ticket.account_id,
        KIND_PASSWORD,
        request,
    )?;
    Ok(Json(
        serde_json::json!({ "new_password_response": response }),
    ))
}

/// Body of `POST /recovery-email/recover/password/finish`.
#[derive(Deserialize)]
pub(crate) struct PasswordFinishReq {
    /// The ticket `verify` returned.
    pub(crate) ticket: String,
    /// Standard base64 of the OPAQUE `RegistrationUpload` for the new
    /// password.
    pub(crate) new_password_upload: String,
    /// URL-safe base64 of the 32-byte id of the device the session lands on.
    /// Absent when the device has no keys yet.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
}

/// Answer of `POST /recovery-email/recover/password/finish`. **No fresh-auth
/// grant**: this type has no field for one.
#[derive(Serialize)]
pub(crate) struct PasswordFinishRes {
    /// URL-safe base64 of the 16-byte account id.
    pub(crate) account_id_b64: String,
    /// The session pair at the account's current key generation.
    #[serde(flatten)]
    pub(crate) session: SessionPair,
}

/// `POST /recovery-email/recover/password/finish` — anonymous, per-address
/// limited. One transaction under the account row's lock:
///
/// 1. the ticket is spent (it must be cleared; of two finishes one deletes
///    it);
/// 2. the recall anchor is captured ([`crate::recall::capture_anchor`]),
///    BEFORE the envelope changes, so the password a recall asks for is the
///    one in force before this;
/// 3. the password envelope is replaced;
/// 4. the account's in-flight password logins are deleted (a login started
///    against the old password must not finish);
/// 5. the "password changed by email" notice is written.
///
/// Then a session pair at the account's generation. The key generation does
/// not move: the account's other devices stay signed in and are told by the
/// notice. No fresh-auth grant is minted.
///
/// # Errors
///
/// `401 ticket-refused`, `403 totp-required`, `400`; [`ApiError::Db`];
/// [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn password_finish(
    State(state): State<AppState>,
    Json(req): Json<PasswordFinishReq>,
) -> Result<Json<PasswordFinishRes>, OpaqueError> {
    let on = enabled(&state)?;
    let upload = decode_registration_upload(&req.new_password_upload)?;
    let device_id = crate::session::decode_device_id(req.device_id_b64.as_deref())?;
    let registration = ServerRegistration::<LakkoCipherSuite>::finish(upload)
        .serialize()
        .to_vec();
    let ticket = load_ticket(&state, &on.config, &req.ticket).await?;
    require_cleared(&ticket)?;
    let account_id = ticket.account_id;
    // ADR-0099 section 4: a suspended account's password is not replaced and
    // the ticket is not spent; the account lands in suspended mode.
    let generation_now = crate::session::key_generation_of(&state.db, &account_id)
        .await
        .map_err(ApiError::Db)?
        .ok_or_else(ticket_refused)?;
    crate::suspension::refuse_landing_if_suspended(
        &state,
        &account_id,
        generation_now,
        &device_id,
        crate::suspension::LandingGrant::None,
    )
    .await?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    let key_generation: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    let key_generation = key_generation.ok_or_else(ticket_refused)?;
    let spent = sqlx::query(
        "DELETE FROM email_tickets WHERE ticket_key = ? \
         AND (totp_required = 0 OR totp_passed = 1)",
    )
    .bind(&ticket.key[..])
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if spent != 1 {
        return Err(ticket_refused().into());
    }
    crate::recall::capture_anchor(
        &mut tx,
        &account_id,
        key_generation,
        crate::recall::Change::Password,
        state.limits.recall_window_ms(),
    )
    .await
    .map_err(ApiError::Db)?;
    sqlx::query(
        "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
         VALUES (?, ?, ?) \
         ON DUPLICATE KEY UPDATE registration_bytes = VALUES(registration_bytes)",
    )
    .bind(&account_id[..])
    .bind(KIND_PASSWORD)
    .bind(&registration)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    sqlx::query("DELETE FROM opaque_login_states WHERE account_id = ? AND kind = ?")
        .bind(&account_id[..])
        .bind(KIND_PASSWORD)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    crate::account_notices::write(
        &mut tx,
        &account_id,
        NoticeKind::PasswordChangedByEmail,
        now_ms(),
    )
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    let account_id_b64 = URL_SAFE_NO_PAD.encode(account_id);
    tracing::info!("recovery email: password replaced");
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;
    Ok(Json(PasswordFinishRes {
        account_id_b64,
        session,
    }))
}

// ---------------------------------------------------------------------------
// The reset factor (ADR-0073, amended 2026-11-26)
// ---------------------------------------------------------------------------

/// Body of `POST /account/keys/reset/factor/email/start`.
#[derive(Deserialize)]
pub(crate) struct ResetStartReq {
    /// The `k1` fresh-auth grant of a PASSWORD sign-in.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// The account's login handle: 32 bytes, base64url without padding.
    pub(crate) handle: String,
    /// The address as the person typed it.
    pub(crate) address: String,
    /// The device's language tag, for the letter. Optional.
    #[serde(default)]
    pub(crate) lang: Option<String>,
}

/// `POST /account/keys/reset/factor/email/start` — bearer + a fresh-auth
/// grant minted by the password.
///
/// Order: the grant (`403 fresh-auth-required`, nothing charged), the
/// feature (`503`), the body (`400`), [`Scope::EmailLetterHandle`] for the
/// handle and [`Scope::EmailLetterAccount`] for the bearer's account (`429
/// email-rate-limited`), then the comparison and the challenge. The answer is
/// the same whether the typed pair is the account's: on a match a challenge
/// of purpose `reset` carries the hash of a code that is mailed (after the
/// answer); otherwise it carries random bytes no code matches. Both replace
/// the bearer's live reset challenge, if any.
///
/// # Errors
///
/// As listed; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn reset_factor_start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<ResetStartReq>,
) -> Result<Json<RecoverStartRes>, ApiError> {
    let account_id = auth.0;
    crate::session::require_password_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;
    let on = enabled(&state)?;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let address = normalise_address(&req.address).ok_or(ApiError::Coded(
        StatusCode::BAD_REQUEST,
        CODE_ADDRESS_INVALID,
    ))?;
    crate::limits::charge_name(&state.db, &state.limits, Scope::EmailLetterHandle, &handle).await?;
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailLetterAccount,
        &account_id,
    )
    .await?;

    let resolved = crate::login_handle::resolve(&state.db, &on.handle_pepper, &handle).await?;
    let stored: Option<[u8; 32]> = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT email_key FROM recovery_emails WHERE account_id = ?",
    )
    .bind(&account_id[..])
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?
    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
    let candidate = on.config.email_key(&handle, &address);
    let reference = stored.unwrap_or_else(|| on.config.dummy_email_key(&handle));
    // Every operand is evaluated; `&` not `&&`.
    #[allow(clippy::needless_bitwise_bool)]
    let send = ct_eq(&candidate, &reference)
        & stored.is_some()
        & resolved.bound
        & (resolved.account_id == account_id);

    let id: [u8; 16] = random();
    let code = generate_code();
    let real_code_key = on.config.hash_code(&id, &code);
    let now = now_ms();
    put_challenge(
        &state.db,
        &NewChallenge {
            id,
            account_id: &account_id,
            purpose: PURPOSE_RESET,
            code_key: if send { real_code_key } else { random() },
            pending_key: None,
            handle_tag: on.config.handle_tag(&handle),
        },
        now,
    )
    .await
    .map_err(ApiError::Db)?;

    if send {
        let config = on.config.clone();
        let letter = Letter {
            to: address,
            code,
            language: clean_language(req.lang),
        };
        // After the answer, so the relay's latency is not the caller's.
        tokio::spawn(async move {
            if let Err(e) = config.sender.send(letter).await {
                tracing::warn!(error = %e, "recovery email: a reset letter was not sent");
            }
        });
    }
    Ok(Json(RecoverStartRes {
        challenge_id: URL_SAFE_NO_PAD.encode(id),
        resend_after_secs: RESEND_AFTER_SECS,
        expires_in_secs: ttl_secs(CODE_TTL_MS),
    }))
}

/// Body of `POST /account/keys/reset/factor/email/finish`.
#[derive(Deserialize)]
pub(crate) struct ResetFinishReq {
    /// The `k1` fresh-auth grant of a PASSWORD sign-in.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
    /// The `challenge_id` the start returned.
    pub(crate) challenge_id: String,
    /// The six digits from the letter.
    pub(crate) code: String,
}

/// `POST /account/keys/reset/factor/email/finish` — bearer + a fresh-auth
/// grant minted by the password. On the right code for a live, real reset
/// challenge of the bearer's account the challenge is consumed and a reset
/// factor grant minted (`factor` `e`). Everything else (a wrong code, a
/// decoy, an unknown, expired, spent or another account's challenge) is the
/// one `401 {"error":"factor-refused"}`; five tries per code. A live
/// challenge charges [`Scope::EmailVerify`] for the handle it was opened for.
///
/// # Errors
///
/// As above; `403 fresh-auth-required`; `503 recovery-email-disabled`;
/// [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn reset_factor_finish(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<ResetFinishReq>,
) -> Result<Json<crate::session::ResetFactorGrant>, ApiError> {
    let account_id = auth.0;
    let generation =
        crate::session::require_password_grant(&state, &account_id, req.fresh_grant.as_deref())
            .await?;
    let on = enabled(&state)?;
    let now = now_ms();
    let challenge = match decode_challenge_id(&req.challenge_id) {
        Some(id) => load_challenge(&state.db, &id, PURPOSE_RESET, now)
            .await
            .map_err(ApiError::Db)?
            .filter(|c| c.account_id == account_id)
            .map(|c| (id, c)),
        None => None,
    };
    let Some((id, challenge)) = challenge else {
        return Err(crate::reset_factor::factor_refused());
    };
    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailVerify,
        &challenge.handle_tag,
    )
    .await?;
    let CodeOutcome::Accepted(_) =
        check_code(&state.db, &on.config, &id, challenge, &req.code, now)
            .await
            .map_err(ApiError::Db)?
    else {
        return Err(crate::reset_factor::factor_refused());
    };
    tracing::info!("reset factor: mailed code accepted");
    Ok(Json(crate::session::issue_reset_factor_grant(
        &state,
        &account_id,
        generation,
        lakko_auth_token::ResetFactor::Email,
    )?))
}

// ---------------------------------------------------------------------------
// Pruner
// ---------------------------------------------------------------------------

/// Every five minutes: challenges and tickets past their lifetime.
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    const JOB: &str = crate::metrics::jobs::RECOVERY_EMAIL_PRUNE;
    lakko_metrics::job_registered(JOB);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = now_ms();
            let mut all_ok = true;
            for statement in [
                "DELETE FROM email_challenges WHERE expires_at_ms <= ?",
                "DELETE FROM email_tickets WHERE expires_at_ms <= ?",
            ] {
                if let Err(e) = sqlx::query(statement).bind(now).execute(&pool).await {
                    all_ok = false;
                    tracing::warn!(error = %e, "recovery email prune failed");
                }
            }
            crate::metrics::job_ran(JOB, all_ok);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::new([0x11; 32], Arc::new(crate::mail::LogOnlySender))
    }

    #[test]
    fn addresses_are_normalised_by_the_one_rule() {
        assert_eq!(
            normalise_address("  Alice.Smith+Tag@Example.COM \n").as_deref(),
            Some("alice.smith+tag@example.com")
        );
        assert_eq!(normalise_address("a@b").as_deref(), Some("a@b"));
        for refused in [
            "",
            "ab",
            "a@",
            "@b",
            "a@b@c",
            "a b@c",
            "a\u{0}@b",
            "no-at-sign",
        ] {
            assert_eq!(normalise_address(refused), None, "{refused:?}");
        }
        let long = format!("{}@b", "a".repeat(253));
        assert_eq!(normalise_address(&long), None);
        let fits = format!("{}@b", "a".repeat(252));
        assert_eq!(normalise_address(&fits).map(|a| a.len()), Some(254));
    }

    #[test]
    fn the_email_key_binds_the_handle_the_address_and_the_pepper() {
        let c = config();
        let key = c.email_key(&[1; 32], "a@b.example");
        assert_eq!(key, c.email_key(&[1; 32], "a@b.example"));
        assert_ne!(key, c.email_key(&[2; 32], "a@b.example"));
        assert_ne!(key, c.email_key(&[1; 32], "a@c.example"));
        let other = Config::new([0x22; 32], Arc::new(crate::mail::LogOnlySender));
        assert_ne!(key, other.email_key(&[1; 32], "a@b.example"));
        // The dummy a missing key is compared with is no address's key.
        assert_ne!(key, c.dummy_email_key(&[1; 32]));
    }

    #[test]
    fn a_code_hash_is_bound_to_its_challenge() {
        let c = config();
        assert_eq!(
            c.hash_code(&[1; 16], "123456"),
            c.hash_code(&[1; 16], "123456")
        );
        assert_ne!(
            c.hash_code(&[1; 16], "123456"),
            c.hash_code(&[2; 16], "123456")
        );
        assert_ne!(
            c.hash_code(&[1; 16], "123456"),
            c.hash_code(&[1; 16], "123457")
        );
    }

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..2000 {
            let code = generate_code();
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn the_rejection_bound_is_the_largest_multiple_of_a_million() {
        const LIMIT: u32 = (u32::MAX / 1_000_000) * 1_000_000;
        assert_eq!(LIMIT, 4_294_000_000);
        assert_eq!(LIMIT % 1_000_000, 0);
    }

    #[test]
    fn language_tags_are_kept_only_when_they_look_like_one() {
        assert_eq!(
            clean_language(Some("pt-BR".into())).as_deref(),
            Some("pt-BR")
        );
        assert_eq!(clean_language(Some("ru\r\nBcc: x".into())), None);
        assert_eq!(clean_language(Some(String::new())), None);
        assert_eq!(clean_language(None), None);
    }
}
