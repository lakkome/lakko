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
//! For an account whose owner has not saved a recovery phrase the server also
//! keeps the **email vault**: the 44-byte blob the client sealed (the phrase
//! entropy under an Argon2id key over the login handle, the address and a
//! server-minted `vault_salt`), wrapped again at rest under
//! `LAKKO_VAULT_ENC_KEY`. It leaves the server only in the answer to a
//! verified mailed code (and authenticator code). **This is the stated
//! exception to invariant #2** (ADR-0075 section 2): the server handles the
//! address in the clear whenever it sends a letter, so an operator who
//! records it can open the vault. Saving a recovery phrase ends the exception
//! for that account by changing its keys (a key rotation with
//! `phrase_saved`, [`crate::key_rotate`]).
//!
//! # When it is on
//!
//! With `LAKKO_AUTH_EMAIL_PEPPER` (64 hex), `LAKKO_AUTH_HANDLE_PEPPER` and
//! `LAKKO_VAULT_ENC_KEY` all set. Otherwise the enrol, remove and recover
//! routes answer `503 {"error":"recovery-email-disabled"}` and
//! `GET /recovery-email/available` answers `{"available": false}`.
//!
//! # Enrol, replace, remove (bearer)
//!
//! - `POST /recovery-email/enrol/start {fresh_grant, handle, address, lang?}`
//!   -> `{challenge_id, vault_salt, resend_after_secs, expires_in_secs}`.
//!   Needs a fresh-auth grant; the handle must be the one bound to the
//!   session's account. The letter is sent before the answer, so a relay
//!   failure is `502 mail-send-failed`.
//! - `POST /recovery-email/enrol/finish {challenge_id, code,
//!   sealed_entropy?}` -> `{enabled, vault}`. Stores `email_key` and, when
//!   `sealed_entropy` (exactly 44 bytes) is present, the vault with the salt
//!   the start minted. Without it any vault the account had is deleted (it
//!   was sealed under the previous address). An account marked
//!   phrase-protected never gets a vault (`409 phrase-saved`).
//! - `DELETE /recovery-email {fresh_grant}` -> `{removed}`.
//! - `GET /recovery-email` -> `{available, enabled, vault, phrase_saved,
//!   totp}`.
//! - `PUT /recovery-email/vault {handle, address, vault_salt,
//!   sealed_entropy}` -> `{vault}` (amended 2026-10-13, no fresh-auth
//!   grant): a device that kept the address re-seals a vault after a reset
//!   or a rotation dropped it. No letter: the server recomputes `email_key`
//!   and requires it to equal the stored one. Only where no vault exists and
//!   no phrase is saved.
//! - `DELETE /recovery-email/vault {fresh_grant}` -> `{vault_removed,
//!   phrase_saved}`: the account becomes phrase-protected.
//! - Re-binding or removing the login handle deletes both rows in the same
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
//!   totp_required}` or the one `401 {"error":"code-refused"}` (wrong code,
//!   decoy, unknown, expired or spent challenge).
//! - `POST /recovery-email/recover/totp {ticket, code}` when required (D8).
//! - `POST /recovery-email/recover/open {ticket}` -> `{mode: "vault",
//!   vault_salt, sealed_entropy}` (the ticket is spent and the account's
//!   devices are told) or `{mode: "password"}`.
//! - `POST /recovery-email/recover/password/{start,finish}` (password mode):
//!   an OPAQUE registration for the password under the ticket. `finish`, in
//!   one transaction under the account row's lock, captures the recall anchor
//!   ([`crate::recall::capture_anchor`]) BEFORE the envelope changes,
//!   replaces the envelope, deletes the in-flight password logins, writes the
//!   notice and lands a session pair. It mints **no fresh-auth grant**: a
//!   mailbox is not the password or a passkey, so whoever came in this way
//!   signs in with the new password before they can reset keys, add a device
//!   or change the email.
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
//! [`Scope::EmailVerify`] per login handle across challenges;
//! [`Scope::EmailVaultOpen`] per account.
//!
//! [ADR-0075]: ../../../docs/adr/0075-recovery-email.md

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
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

/// The exact size of an email-vault blob as the client seals it:
/// 12 (nonce) + 16 (phrase entropy) + 16 (tag), the passkey vault's shape.
pub(crate) const SEALED_ENTROPY_LEN: usize = lakko_mnemonic::vault::EMAIL_VAULT_SEALED_LEN;

/// Length of the per-vault salt minted at enrol start.
pub(crate) const VAULT_SALT_LEN: usize = lakko_mnemonic::vault::EMAIL_VAULT_SALT_LEN;

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

/// How often expired challenges and tickets are deleted.
const PRUNE_INTERVAL: Duration = Duration::from_secs(300);

/// Body codes of this module's refusals.
const CODE_DISABLED: &str = "recovery-email-disabled";
const CODE_CODE_REFUSED: &str = "code-refused";
const CODE_CODE_EXPIRED: &str = "code-expired";
const CODE_TICKET_REFUSED: &str = "ticket-refused";
const CODE_TOTP_REQUIRED: &str = "totp-required";
const CODE_TOTP_DISABLED: &str = "totp-disabled";
const CODE_WRONG_MODE: &str = "wrong-mode";
const CODE_ADDRESS_INVALID: &str = "address-invalid";
const CODE_HANDLE_MISMATCH: &str = "handle-mismatch";
const CODE_USERNAME_REQUIRED: &str = "username-required";
const CODE_PHRASE_SAVED: &str = "phrase-saved";
const CODE_MAIL_SEND_FAILED: &str = "mail-send-failed";
const CODE_ADDRESS_MISMATCH: &str = "address-mismatch";
const CODE_NO_RECOVERY_EMAIL: &str = "no-recovery-email";
const CODE_VAULT_EXISTS: &str = "vault-exists";

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
        Some(_) if state.vault_enc_key.is_none() || state.handle_pepper.is_none() => {
            tracing::warn!(
                "recovery email DISABLED: it also needs LAKKO_VAULT_ENC_KEY and \
                 LAKKO_AUTH_HANDLE_PEPPER; its routes answer 503"
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
    vault_key: [u8; 32],
    handle_pepper: [u8; 32],
}

/// The configuration, or the `503` every gated route answers without it.
fn enabled(state: &AppState) -> Result<Enabled, ApiError> {
    match (
        state.recovery_email.as_ref(),
        state.vault_enc_key.as_ref(),
        state.handle_pepper.as_ref(),
    ) {
        (Some(config), Some(vault_key), Some(handle_pepper)) => Ok(Enabled {
            config: config.clone(),
            vault_key: **vault_key,
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
/// rules). The client derives the vault key from the same function's output,
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
    vault_salt: Option<[u8; VAULT_SALT_LEN]>,
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
            (id, account_id, purpose, code_key, pending_key, vault_salt, handle_tag, attempts, \
             expires_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?) \
         ON DUPLICATE KEY UPDATE \
            id = VALUES(id), code_key = VALUES(code_key), pending_key = VALUES(pending_key), \
            vault_salt = VALUES(vault_salt), handle_tag = VALUES(handle_tag), attempts = 0, \
            expires_at_ms = VALUES(expires_at_ms)",
    )
    .bind(&challenge.id[..])
    .bind(&challenge.account_id[..])
    .bind(challenge.purpose)
    .bind(&challenge.code_key[..])
    .bind(challenge.pending_key.as_ref().map(|k| &k[..]))
    .bind(challenge.vault_salt.as_ref().map(|s| &s[..]))
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
    vault_salt: Option<[u8; VAULT_SALT_LEN]>,
    handle_tag: [u8; 32],
}

type ChallengeRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, Vec<u8>);

/// The unexpired challenge `id` of `purpose`, if any.
async fn load_challenge(
    pool: &MySqlPool,
    id: &[u8; 16],
    purpose: u8,
    now: i64,
) -> Result<Option<Challenge>, sqlx::Error> {
    let row: Option<ChallengeRow> = sqlx::query_as(
        "SELECT account_id, code_key, pending_key, vault_salt, handle_tag \
         FROM email_challenges WHERE id = ? AND purpose = ? AND expires_at_ms > ?",
    )
    .bind(&id[..])
    .bind(purpose)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(account, code_key, pending, salt, tag)| {
        Some(Challenge {
            account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
            code_key: <[u8; 32]>::try_from(code_key.as_slice()).ok()?,
            pending_key: pending.and_then(|p| <[u8; 32]>::try_from(p.as_slice()).ok()),
            vault_salt: salt.and_then(|s| <[u8; VAULT_SALT_LEN]>::try_from(s.as_slice()).ok()),
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
#[allow(clippy::struct_excessive_bools)] // four independent facts on the wire, not a state
pub(crate) struct StatusRes {
    /// Whether this server can send a recovery code.
    pub(crate) available: bool,
    /// The account has a recovery email. Never the address.
    pub(crate) enabled: bool,
    /// The account has an email vault: a mailed code brings its keys back.
    pub(crate) vault: bool,
    /// The owner saved a recovery phrase: a mailed code changes the password
    /// only, and the account never gets a vault.
    pub(crate) phrase_saved: bool,
    /// The account has a confirmed authenticator: a mailed code will not be
    /// enough without its code (D8), which the enrolment screens say.
    pub(crate) totp: bool,
}

/// `(phrase_saved, has email, has vault, has confirmed authenticator)` of an
/// account, as [`status`] reads it.
type StatusRow = (bool, Option<i64>, Option<i64>, Option<i64>);

/// `GET /recovery-email` — bearer. The session's account only. Not gated on
/// the pepper: `phrase_saved` is meaningful on a server that sends no mail.
///
/// # Errors
///
/// [`ApiError::Db`]; [`ApiError::InvalidCredential`] when the account is gone.
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<StatusRes>, ApiError> {
    let row: Option<StatusRow> = sqlx::query_as(
        "SELECT a.phrase_saved, \
                (SELECT 1 FROM recovery_emails e WHERE e.account_id = a.id), \
                (SELECT 1 FROM email_vaults v WHERE v.account_id = a.id), \
                (SELECT 1 FROM totp_secrets t \
                  WHERE t.account_id = a.id AND t.confirmed_at IS NOT NULL) \
         FROM accounts a WHERE a.id = ?",
    )
    .bind(&auth.0[..])
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let (phrase_saved, email, vault, totp) = row.ok_or(ApiError::InvalidCredential)?;
    Ok(Json(StatusRes {
        available: is_available(&state),
        enabled: email.is_some(),
        vault: vault.is_some(),
        phrase_saved,
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
    /// Standard base64 of the 16-byte salt the client seals the vault with.
    pub(crate) vault_salt: String,
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
    let vault_salt: [u8; VAULT_SALT_LEN] = random();
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
            vault_salt: Some(vault_salt),
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
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "recovery email: enrol code sent"
    );
    Ok(Json(EnrolStartRes {
        challenge_id: URL_SAFE_NO_PAD.encode(id),
        vault_salt: B64.encode(vault_salt),
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
    /// Standard base64 of the 44-byte email vault, sealed with the start's
    /// `vault_salt`. Present for an account without a saved phrase; absent
    /// otherwise.
    #[serde(default)]
    pub(crate) sealed_entropy: Option<String>,
}

/// Answer of `POST /recovery-email/enrol/finish`.
#[derive(Serialize)]
pub(crate) struct EnrolFinishRes {
    /// Always `true`: the account has a recovery email.
    pub(crate) enabled: bool,
    /// Whether a vault was stored with it.
    pub(crate) vault: bool,
}

/// `POST /recovery-email/enrol/finish` — bearer.
///
/// A wrong code is `401 code-refused` (five tries); a challenge that is not
/// there (expired, spent, another account's, replaced by a newer start) is
/// `410 code-expired`. On the right code, one transaction under the account
/// row's lock: the `email_key` is stored (replacing the previous address's),
/// the vault is stored or, without `sealed_entropy`, deleted, the account's
/// recovery tickets are deleted, and the "added" / "replaced" notice written.
///
/// # Errors
///
/// As above; `409 phrase-saved` for a vault offered by a phrase-protected
/// account; [`ApiError::BadInput`]; [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
#[allow(clippy::too_many_lines)] // the code check, then one transaction read top to bottom
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
    let sealed = req
        .sealed_entropy
        .as_deref()
        .map(|b64| {
            B64.decode(b64)
                .ok()
                .filter(|bytes| bytes.len() == SEALED_ENTROPY_LEN)
                .ok_or(ApiError::BadInput(
                    "sealed_entropy must be the 44-byte blob of a 12-word phrase, base64",
                ))
        })
        .transpose()?;

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
    let (Some(email_key), Some(vault_salt)) = (challenge.pending_key, challenge.vault_salt) else {
        tracing::error!("recovery email: an enrol challenge without its pending key");
        return Err(ApiError::Internal);
    };
    let body_enc = sealed
        .as_deref()
        .map(|sealed| {
            lakko_crypto::aead::seal_with_key(&on.vault_key, sealed).map_err(|e| {
                tracing::error!(error = %e, "recovery email: vault seal failed");
                ApiError::Internal
            })
        })
        .transpose()?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    let phrase_saved: Option<bool> =
        sqlx::query_scalar("SELECT phrase_saved FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    let phrase_saved = phrase_saved.ok_or(ApiError::InvalidCredential)?;
    if phrase_saved && body_enc.is_some() {
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_PHRASE_SAVED));
    }
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
    match body_enc.as_deref() {
        Some(body_enc) => {
            sqlx::query(
                "INSERT INTO email_vaults (account_id, vault_salt, body_enc) VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE vault_salt = VALUES(vault_salt), \
                    body_enc = VALUES(body_enc), created_at = NOW(3)",
            )
            .bind(&account_id[..])
            .bind(&vault_salt[..])
            .bind(body_enc)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
        },
        None => {
            // A vault the account had was sealed under the previous address.
            sqlx::query("DELETE FROM email_vaults WHERE account_id = ?")
                .bind(&account_id[..])
                .execute(&mut *tx)
                .await
                .map_err(ApiError::Db)?;
        },
    }
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

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        vault = body_enc.is_some(),
        replaced = had.is_some(),
        "recovery email: enrolled"
    );
    Ok(Json(EnrolFinishRes {
        enabled: true,
        vault: body_enc.is_some(),
    }))
}

/// Body of `DELETE /recovery-email` and `DELETE /recovery-email/vault`.
#[derive(Deserialize)]
pub(crate) struct GrantReq {
    /// The `k1` fresh-auth grant.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
}

/// Delete everything the recovery email keeps for `account_id`: the key, the
/// vault, the tickets and the challenges. Returns whether there was a key.
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
        "DELETE FROM email_vaults WHERE account_id = ?",
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
/// `email_key` (and the vault key the client derives) was made from the old
/// name, so the recovery email is turned off in the SAME transaction
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
    /// Standard base64 of the 16-byte salt of the vault re-sealed under the
    /// NEW handle. With `sealed_entropy`, or neither.
    #[serde(default)]
    pub(crate) vault_salt: Option<String>,
    /// Standard base64 of the 44-byte vault re-sealed under the NEW handle.
    #[serde(default)]
    pub(crate) sealed_entropy: Option<String>,
}

/// What a handle change does to the recovery email, computed before the
/// bind's transaction: everything but the comparison with the stored row.
pub(crate) struct RekeyPlan {
    /// `email_key` of the address under the handle the client says the
    /// account holds now (`previous_handle`); `None` when it sent none.
    old_email_key: Option<[u8; 32]>,
    /// `email_key` of the address under the NEW handle.
    new_email_key: [u8; 32],
    /// The vault to store, already wrapped at rest: `(vault_salt, body_enc)`.
    vault: Option<([u8; VAULT_SALT_LEN], Vec<u8>)>,
}

/// How a re-key inside the bind's transaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RekeyOutcome {
    /// The account has no recovery email: nothing to do.
    Nothing,
    /// The key now names the new handle (and the vault was replaced or
    /// removed).
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
/// wish would be worse than refusing); `400 address-invalid`; `400` for a
/// salt without a blob, the reverse, or either of the wrong size;
/// [`ApiError::Internal`].
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
    let vault = match (req.vault_salt.as_deref(), req.sealed_entropy.as_deref()) {
        (None, None) => None,
        (Some(salt), Some(sealed)) => {
            let salt: [u8; VAULT_SALT_LEN] = B64
                .decode(salt)
                .ok()
                .and_then(|bytes| <[u8; VAULT_SALT_LEN]>::try_from(bytes.as_slice()).ok())
                .ok_or(ApiError::BadInput("vault_salt must be 16 bytes, base64"))?;
            let sealed = B64
                .decode(sealed)
                .ok()
                .filter(|bytes| bytes.len() == SEALED_ENTROPY_LEN)
                .ok_or(ApiError::BadInput(
                    "sealed_entropy must be the 44-byte blob of a 12-word phrase, base64",
                ))?;
            let body_enc =
                lakko_crypto::aead::seal_with_key(&on.vault_key, &sealed).map_err(|e| {
                    tracing::error!(error = %e, "recovery email: vault seal failed");
                    ApiError::Internal
                })?;
            Some((salt, body_enc))
        },
        _ => {
            return Err(ApiError::BadInput(
                "vault_salt and sealed_entropy go together",
            ))
        },
    };
    Ok(RekeyPlan {
        old_email_key: previous_handle.map(|old| on.config.email_key(old, &address)),
        new_email_key: on.config.email_key(new_handle, &address),
        vault,
    })
}

/// Move the account's recovery email to its new handle inside the bind's
/// transaction (the caller holds the account row's lock and has checked that
/// `previous_handle` is the handle the account holds).
///
/// With no `recovery_emails` row there is nothing to move. Otherwise the
/// address under the previous handle must be the stored key's (compared
/// without an early exit); then the key is rewritten under the new handle,
/// the vault is replaced by the one sent (never for a phrase-protected
/// account) or removed (the old one was sealed under the old handle), and
/// the account's challenges and tickets are dropped. No notice: the
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
    let phrase_saved: bool = sqlx::query_scalar("SELECT phrase_saved FROM accounts WHERE id = ?")
        .bind(&account_id[..])
        .fetch_one(&mut **tx)
        .await?;
    match plan.vault.as_ref().filter(|_| !phrase_saved) {
        Some((salt, body_enc)) => {
            sqlx::query(
                "INSERT INTO email_vaults (account_id, vault_salt, body_enc) VALUES (?, ?, ?) \
                 ON DUPLICATE KEY UPDATE vault_salt = VALUES(vault_salt), \
                    body_enc = VALUES(body_enc), created_at = NOW(3)",
            )
            .bind(&account_id[..])
            .bind(&salt[..])
            .bind(body_enc.as_slice())
            .execute(&mut **tx)
            .await?;
        },
        None => {
            sqlx::query("DELETE FROM email_vaults WHERE account_id = ?")
                .bind(&account_id[..])
                .execute(&mut **tx)
                .await?;
        },
    }
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
/// Removes the key and the vault. `removed` says whether there was one.
///
/// # Errors
///
/// `503 recovery-email-disabled`, [`ApiError::FreshAuthRequired`],
/// [`ApiError::Db`].
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
    let removed = delete_rows(&mut tx, &account_id)
        .await
        .map_err(ApiError::Db)?;
    if removed {
        crate::account_notices::write(&mut tx, &account_id, NoticeKind::EmailRemoved, now_ms())
            .await
            .map_err(ApiError::Db)?;
    }
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        removed,
        "recovery email: removed"
    );
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

/// Body of `PUT /recovery-email/vault`.
#[derive(Deserialize)]
pub(crate) struct PutVaultReq {
    /// The account's login handle: 32 bytes, base64url without padding. The
    /// server stores only a keyed hash of it, so it cannot recompute
    /// `email_key` without being handed the handle; it checks that this is
    /// the one bound to the session's account.
    pub(crate) handle: String,
    /// The address the account already confirmed, as the device kept it.
    pub(crate) address: String,
    /// Standard base64 of the 16-byte salt the client sealed the vault with.
    pub(crate) vault_salt: String,
    /// Standard base64 of the 44-byte email vault.
    pub(crate) sealed_entropy: String,
}

/// `PUT /recovery-email/vault` — bearer, NO fresh-auth grant (ADR-0075,
/// amended 2026-10-13). A device of the account that kept the address stores
/// a vault again after a key reset or a rotation dropped it. No letter is
/// sent: the address is the one already confirmed, which the server checks
/// by recomputing `email_key` from the bound handle and the address.
///
/// Order: the feature (`503`); the body (`400`, `address-invalid` for a
/// refused address); [`Scope::EmailVaultStore`] for the account (`429`),
/// before anything is compared, so a session cannot test addresses freely;
/// the handle is the account's bound one (`409 username-required` /
/// `handle-mismatch`); then one transaction under the account row's lock:
/// a stored key exists (`409 no-recovery-email`) and equals the recomputed
/// one, compared without an early exit (`403 address-mismatch`); no phrase
/// is saved (`409 phrase-saved`); no vault exists (`409 vault-exists`); the
/// vault is stored under the client's salt, wrapped at rest.
///
/// It can only add a vault where there is none, never replace one, and it
/// writes no notice. A vault sealing the wrong entropy fails the restore's
/// bundle check on the client: a recovery that does not work, not a
/// takeover.
///
/// # Errors
///
/// As listed; [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn put_vault(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<PutVaultReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let on = enabled(&state)?;
    let account_id = auth.0;
    let handle = crate::login_handle::decode_handle(&req.handle)?;
    let address = normalise_address(&req.address).ok_or(ApiError::Coded(
        StatusCode::BAD_REQUEST,
        CODE_ADDRESS_INVALID,
    ))?;
    let vault_salt: [u8; VAULT_SALT_LEN] = B64
        .decode(&req.vault_salt)
        .ok()
        .and_then(|bytes| <[u8; VAULT_SALT_LEN]>::try_from(bytes.as_slice()).ok())
        .ok_or(ApiError::BadInput("vault_salt must be 16 bytes, base64"))?;
    let sealed = B64
        .decode(&req.sealed_entropy)
        .ok()
        .filter(|bytes| bytes.len() == SEALED_ENTROPY_LEN)
        .ok_or(ApiError::BadInput(
            "sealed_entropy must be the 44-byte blob of a 12-word phrase, base64",
        ))?;

    crate::limits::charge_name(
        &state.db,
        &state.limits,
        Scope::EmailVaultStore,
        &account_id,
    )
    .await?;

    require_own_handle(&state, &on, &account_id, &handle).await?;
    let candidate = on.config.email_key(&handle, &address);
    let body_enc = lakko_crypto::aead::seal_with_key(&on.vault_key, &sealed).map_err(|e| {
        tracing::error!(error = %e, "recovery email: vault seal failed");
        ApiError::Internal
    })?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    let phrase_saved: Option<bool> =
        sqlx::query_scalar("SELECT phrase_saved FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    let phrase_saved = phrase_saved.ok_or(ApiError::InvalidCredential)?;
    let stored: Option<[u8; 32]> = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT email_key FROM recovery_emails WHERE account_id = ?",
    )
    .bind(&account_id[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::Db)?
    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
    let Some(stored) = stored else {
        return Err(ApiError::Coded(
            StatusCode::CONFLICT,
            CODE_NO_RECOVERY_EMAIL,
        ));
    };
    if !ct_eq(&candidate, &stored) {
        return Err(ApiError::Coded(
            StatusCode::FORBIDDEN,
            CODE_ADDRESS_MISMATCH,
        ));
    }
    if phrase_saved {
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_PHRASE_SAVED));
    }
    // Under the account row's lock every writer of this account's vault
    // waits, so the read and the insert see the same state.
    let has_vault: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM email_vaults WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    if has_vault.is_some() {
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_VAULT_EXISTS));
    }
    sqlx::query("INSERT INTO email_vaults (account_id, vault_salt, body_enc) VALUES (?, ?, ?)")
        .bind(&account_id[..])
        .bind(&vault_salt[..])
        .bind(body_enc.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "recovery email: vault stored again for the confirmed address"
    );
    Ok(Json(serde_json::json!({ "vault": true })))
}

/// `DELETE /recovery-email/vault {fresh_grant}` — bearer + fresh-auth grant.
/// The account becomes phrase-protected: its email vault is deleted, it is
/// marked so that it never gets one again, and its devices get the "recovery
/// phrase saved" notice (once). The `email_key` is kept: a mailed code still
/// changes the password.
///
/// For an account whose phrase was never in a vault (a sign-up that chose
/// "Save a recovery phrase"). An account that HAD a vault saves a phrase by
/// rotating its keys with `phrase_saved: true` ([`crate::key_rotate`]), since
/// a copy of the vault taken earlier still opens the old phrase.
///
/// Not gated on the pepper: a phrase is saved on a server that sends no mail
/// too.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`], [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn remove_vault(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<GrantReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;
    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    lock_account(&mut tx, &account_id).await?;
    let vault_removed = sqlx::query("DELETE FROM email_vaults WHERE account_id = ?")
        .bind(&account_id[..])
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?
        .rows_affected()
        > 0;
    crate::account_notices::mark_phrase_saved(&mut tx, &account_id, now_ms())
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        vault_removed,
        "recovery email: account marked phrase-protected"
    );
    Ok(Json(serde_json::json!({
        "vault_removed": vault_removed,
        "phrase_saved": true,
    })))
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
            vault_salt: None,
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
    /// `open` or the password routes answer.
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
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        totp_required,
        "recovery email: code accepted"
    );
    Ok(Json(RecoverVerifyRes {
        ticket: URL_SAFE_NO_PAD.encode(ticket),
        totp_required,
        expires_in_secs: ttl_secs(TICKET_TTL_MS),
    }))
}

// ---------------------------------------------------------------------------
// Recover: tickets, totp, open
// ---------------------------------------------------------------------------

/// A live ticket as the routes read it.
struct Ticket {
    key: [u8; 32],
    account_id: [u8; 16],
    totp_required: bool,
    totp_passed: bool,
    password_mode: bool,
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
    let row: Option<(Vec<u8>, bool, bool, bool)> = sqlx::query_as(
        "SELECT account_id, totp_required, totp_passed, password_mode \
         FROM email_tickets WHERE ticket_key = ? AND expires_at_ms > ?",
    )
    .bind(&key[..])
    .bind(now_ms())
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::Db)?;
    row.and_then(|(account, totp_required, totp_passed, password_mode)| {
        Some(Ticket {
            key,
            account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
            totp_required,
            totp_passed,
            password_mode,
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

/// Body of `POST /recovery-email/recover/open` and of the password start.
#[derive(Deserialize)]
pub(crate) struct RecoverOpenReq {
    /// The ticket `verify` returned.
    pub(crate) ticket: String,
}

/// `POST /recovery-email/recover/open` — anonymous, per-address limited.
///
/// With an email vault: charges [`Scope::EmailVaultOpen`] for the account,
/// then in one transaction spends the ticket and writes the "account opened
/// by email" notice, and answers `{"mode":"vault","vault_salt",
/// "sealed_entropy"}` (both standard base64). The client opens the vault
/// with the username and address the person typed and continues with the
/// ordinary phrase restore.
///
/// Without one: marks the ticket for the password routes and answers
/// `{"mode":"password"}`.
///
/// Either way only after the authenticator step, when the ticket requires
/// it (`403 totp-required`).
///
/// # Errors
///
/// `401 ticket-refused`, `403 totp-required`, `429 too-many-attempts`;
/// [`ApiError::Db`]; [`ApiError::Internal`].
#[axum::debug_handler]
pub(crate) async fn recover_open(
    State(state): State<AppState>,
    Json(req): Json<RecoverOpenReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let on = enabled(&state)?;
    let ticket = load_ticket(&state, &on.config, &req.ticket).await?;
    if !ticket.cleared() {
        return Err(ApiError::Coded(StatusCode::FORBIDDEN, CODE_TOTP_REQUIRED));
    }
    let account_id = ticket.account_id;
    let vault: Option<(Vec<u8>, Vec<u8>)> =
        sqlx::query_as("SELECT vault_salt, body_enc FROM email_vaults WHERE account_id = ?")
            .bind(&account_id[..])
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::Db)?;
    let Some((vault_salt, body_enc)) = vault else {
        sqlx::query("UPDATE email_tickets SET password_mode = 1 WHERE ticket_key = ?")
            .bind(&ticket.key[..])
            .execute(&state.db)
            .await
            .map_err(ApiError::Db)?;
        return Ok(Json(serde_json::json!({ "mode": "password" })));
    };

    crate::limits::charge_name(&state.db, &state.limits, Scope::EmailVaultOpen, &account_id)
        .await?;
    let sealed = lakko_crypto::aead::open_with_key(&on.vault_key, &body_enc).map_err(|e| {
        tracing::error!(error = %e, "recovery email: vault open failed");
        ApiError::Internal
    })?;

    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
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
        return Err(ticket_refused());
    }
    crate::account_notices::write(
        &mut tx,
        &account_id,
        NoticeKind::AccountOpenedByEmail,
        now_ms(),
    )
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        "recovery email: vault released"
    );
    Ok(Json(serde_json::json!({
        "mode": "vault",
        "vault_salt": B64.encode(vault_salt),
        "sealed_entropy": B64.encode(sealed),
    })))
}

// ---------------------------------------------------------------------------
// Recover: password mode
// ---------------------------------------------------------------------------

/// The gates of both password routes: the authenticator step (`403
/// totp-required`), then that `open` answered the password mode for this
/// ticket (`409 wrong-mode`).
fn require_password_mode(ticket: &Ticket) -> Result<(), ApiError> {
    if !ticket.cleared() {
        return Err(ApiError::Coded(StatusCode::FORBIDDEN, CODE_TOTP_REQUIRED));
    }
    if !ticket.password_mode {
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_WRONG_MODE));
    }
    Ok(())
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
/// `401 ticket-refused`, `403 totp-required`, `409 wrong-mode`, `400` for a
/// request that does not decode; [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn password_start(
    State(state): State<AppState>,
    Json(req): Json<PasswordStartReq>,
) -> Result<Json<serde_json::Value>, OpaqueError> {
    let on = enabled(&state)?;
    let request = decode_registration_request(&req.new_password_request)?;
    let ticket = load_ticket(&state, &on.config, &req.ticket).await?;
    require_password_mode(&ticket)?;
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
/// 1. the ticket is spent (it must be cleared and in password mode; of two
///    finishes one deletes it);
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
/// `401 ticket-refused`, `403 totp-required`, `409 wrong-mode`, `400`;
/// [`ApiError::Db`]; [`ApiError::Internal`].
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
    require_password_mode(&ticket)?;
    let account_id = ticket.account_id;

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
        "DELETE FROM email_tickets WHERE ticket_key = ? AND password_mode = 1 \
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
    tracing::info!(account_id = %account_id_b64, "recovery email: password replaced");
    let session = crate::session::issue_pair_at(&state, &account_id, key_generation, &device_id)?;
    Ok(Json(PasswordFinishRes {
        account_id_b64,
        session,
    }))
}

// ---------------------------------------------------------------------------
// Pruner
// ---------------------------------------------------------------------------

/// Every five minutes: challenges and tickets past their lifetime.
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = now_ms();
            for statement in [
                "DELETE FROM email_challenges WHERE expires_at_ms <= ?",
                "DELETE FROM email_tickets WHERE expires_at_ms <= ?",
            ] {
                if let Err(e) = sqlx::query(statement).bind(now).execute(&pool).await {
                    tracing::warn!(error = %e, "recovery email prune failed");
                }
            }
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
