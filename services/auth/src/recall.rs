// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The recall: taking the account back from an old device after a key reset
//! ([ADR-0074] sections 0.2 and 1; built in sprint T3, see the ADR's "T3 as
//! built").
//!
//! A key reset is instant, and the server cannot tell the owner who lost a
//! phone from someone who learned the password. What the owner has that the
//! other party does not is a device from BEFORE the change. This module lets
//! that device take the account back, with two proofs together:
//!
//! - the **anchor key**: the account's identity key as it was before the
//!   change, which the old device still holds;
//! - the **anchored password**: the password as it was before the first
//!   change made while that key was current.
//!
//! Neither alone is enough. The key alone is a found phone; the password
//! alone is what the attacker holds.
//!
//! # Anchors and the capture rule
//!
//! `recall_anchors` holds, per account, an ordered list of what the account
//! was before each change of standing. [`capture_anchor`] runs inside every
//! transaction that replaces the password envelope or the identity key
//! (`/password/reset/finish`, `/account/keys/reset/finish`; **not** the
//! recall itself), under the account row's lock and before the change:
//!
//! - no recorded identity key: nothing;
//! - an unexpired anchor already exists for the CURRENT identity key: its
//!   contents are left alone (only its expiry is pushed out when the
//!   identity key is about to change);
//! - otherwise one is inserted: the current identity key, the current
//!   password envelope, expiry now + the window.
//!
//! So there is at most one anchor per identity key and it holds the account
//! as it was before the FIRST change made under that key. The party who makes
//! the change cannot choose what a recall is checked against: the key was
//! recorded before they acted and the envelope was the one in force before
//! their first act. Two concurrent changes capture exactly one anchor (the
//! row lock serialises them; the unique key is the backstop).
//!
//! Anchors are ordered by `seq`, and an OLDER standing outranks a newer one:
//! a recall through anchor `i` deletes `i` and every higher one and keeps the
//! lower ones. A recall captures nothing. So the party a recall displaces
//! cannot recall back (their key was current and has no anchor), an attacker
//! who resets twice cannot make their own first key the winning one, and
//! every recall strictly shortens the list.
//!
//! # The routes
//!
//! All three are anonymous: the old device's session was superseded by the
//! reset. All three sit behind the per-address bucket.
//!
//! 1. `POST /account/recall/status {statement_b64}`: is there an anchor for
//!    my key, and until when.
//! 2. `POST /account/recall/start`: one password attempt. Runs the OPAQUE
//!    login start against the ANCHORED envelope and the two registration
//!    starts (new password, new recovery credential), and returns a server
//!    nonce and a session id.
//! 3. `POST /account/recall/finish`: the OPAQUE login finish, then the
//!    reset's transaction with a new password, and a session.
//!
//! **Every request is first checked for a statement signed by the anchor's
//! key**, and everything that is not a wrong password is answered with one
//! body, `401 {"error":"recall-refused"}`: an unknown account, no anchor, an
//! expired or outranked anchor, a malformed or stale statement, a statement
//! by another key, a replayed or unknown session. A caller without the key
//! learns nothing, not even whether the account exists, and reaches neither
//! the password check nor any counter.
//!
//! # Why the START is the step that is authenticated and counted
//!
//! The ADR's sketch left `start` unauthenticated and counted wrong passwords
//! at `finish`. That does not hold for OPAQUE: the server's answer to a login
//! start is what lets a client test one password guess, locally, before it
//! sends anything else. A client with a wrong password never needs to call
//! `finish` at all. So here:
//!
//! - `start` requires a `recall-start` account statement (kind 5) by the
//!   anchor's key over the anchor's `seq` and the login request;
//! - each accepted `start` is charged to [`Scope::Recall`] (5 per 15 minutes
//!   per anchor by default) and counted in `recall_anchors.attempts`; at
//!   [`MAX_ATTEMPTS`] the anchor is destroyed.
//!
//! Only a holder of the anchor's key can therefore spend either budget. A
//! stranger who knows the account id can spend nothing and cannot lock the
//! owner out of the recall.
//!
//! # A wrong clock is not a missing anchor
//!
//! A statement is good for two minutes either side of the server's clock. A
//! device whose clock is off by more would be told `recall-refused`, read
//! that as "no anchor for my key" and destroy its recall record: the one
//! thing that could have taken the account back. So a statement that is
//! right in every way but its time (the signature verifies under the
//! anchor's key, the kind, the account and the body match) is answered `401
//! {"error":"statement-stale","server_time_ms":...}` instead, on all three
//! routes. The device signs again with the server's time. Nothing is counted
//! and nothing is consumed by a stale request. The answer is given only when
//! the signature verified under the anchor's key, so it tells a caller
//! without that key nothing: to them a stale statement is `recall-refused`
//! like any other.
//!
//! # A lost answer can be asked again
//!
//! `finish` answers with the only credentials the recalling device will ever
//! get. If that answer is lost on the way back, the device would hold a new
//! phrase the server has accepted and no session. A successful `finish`
//! therefore leaves a row in `recall_outcomes` for what is left of the
//! attempt's lifetime: the attempt's id, BLAKE3 over the whole request, the
//! generation the recall set, and the attestation. A second `finish` naming
//! that attempt with **exactly the same request** is answered again: the
//! same attestation, and a pair and a fresh-auth grant issued at that
//! generation. Honoured only while the account is still at that generation,
//! so it can undo nothing that happened since. Any other second `finish` for
//! the attempt is `recall-refused`.
//!
//! Who can use it: someone who holds the exact request, statement and OPAQUE
//! finalization included. That is the device that sent it, or someone who
//! could read the first exchange in full and so already had its answer. The
//! replay changes no account state: it issues credentials for the standing
//! the recall created.
//!
//! # The phrase arm (ADR-0075 section 8)
//!
//! A person who holds the recovery phrase has the old seed without an old
//! device, and may not know the old password. So an anchor also keeps the
//! **recovery envelope** in force at capture, and `start` takes
//! `proof: "phrase"`: the OPAQUE login is then run against the anchored
//! recovery envelope instead of the password one. Everything else is the
//! same: the statement by the anchor's key, the counters, the finish.
//!
//! The arm exists only for an account that had **no email vault** when the
//! anchor was captured (`recall_anchors.mail_protected = 0`). With a vault,
//! whoever recovers by mail holds the same phrase as the owner, so the
//! phrase proves nothing; there the answer is the uniform `recall-refused`,
//! and the owner has the password arm from an old device.
//!
//! A device with no cached account id names the account by **login handle**
//! on `status` (`handle`): the statement it signs then carries the first 16
//! bytes of the handle where the account id goes (it cannot know the id),
//! and the answer, given only to a statement that verifies under an anchor's
//! key, carries the account id for `start` and `finish`.
//!
//! [ADR-0074]: ../../../docs/adr/0074-account-takeover-resistance.md

use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_attest::{
    account_statement_is_stale, recall_body, recall_start_body, verify_account_statement,
    AccountStatement, AccountStatementError, AccountStatementKind, KeyResetAttestation,
};
use lakko_crypto::signing::{SigningKey, VerifyingKey};
use opaque_ke::{
    CredentialFinalization, CredentialRequest, ServerLogin, ServerLoginStartParameters,
    ServerRegistration,
};
use rand::rngs::OsRng;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sqlx::{MySql, MySqlPool, Transaction};
use uuid::Uuid;

use crate::limits::Scope;
use crate::opaque::{
    build_credential_id, decode_registration_request, decode_registration_upload,
    opaque_registration_start, LakkoCipherSuite, OpaqueError, KIND_PASSWORD, KIND_RECOVERY,
};
use crate::session::{now_ms, FreshGrant, SessionPair};
use crate::webauthn::ApiError;
use crate::AppState;

/// At most this many unexpired anchors per account (ADR-0074 D4; chosen, not
/// measured). Past it a change captures nothing and the older anchors stand.
pub(crate) const MAX_ANCHORS: usize = 8;

/// Password attempts an anchor admits before it is destroyed (ADR-0074 D4;
/// chosen, not measured). One per accepted `recall/start`.
pub(crate) const MAX_ATTEMPTS: u32 = 20;

/// How long after a `recall/start` its `recall/finish` is accepted. The
/// signup's value, not the login's two minutes: between the two calls the
/// client runs three Argon2id stretches (the old password, the new password,
/// the new recovery seed).
pub(crate) const RECALL_STATE_TTL_MS: i64 = 600_000;

/// How often the pruner deletes expired anchors and stale recall states.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// What a transaction is about to change; decides whether an existing
/// anchor's expiry is pushed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The password envelope only (`/password/reset/finish`).
    Password,
    /// The identity key (`/account/keys/reset/finish`).
    IdentityKey,
}

/// Run the capture rule (the module doc) for `account_id`.
///
/// **The caller must hold the account row's lock** in `tx`
/// (`SELECT ... FROM accounts WHERE id = ? FOR UPDATE`) and must call this
/// BEFORE it changes the password envelope or the identity key. The lock is
/// what makes two concurrent changes capture one anchor.
///
/// Captures nothing when the account has no recorded identity key, when it
/// has no password envelope (nothing a recall could be checked against), or
/// when [`MAX_ANCHORS`] unexpired anchors already exist.
///
/// # Errors
///
/// Any `sqlx::Error`; the caller rolls its whole change back, so a change
/// never lands without its capture.
#[allow(clippy::too_many_lines)] // the capture rule, one branch after another under one lock
pub(crate) async fn capture_anchor(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    key_generation: u64,
    change: Change,
    window_ms: i64,
) -> Result<(), sqlx::Error> {
    // Every read in here is a LOCKING read. A plain read would give the
    // transaction a snapshot, and with MariaDB's snapshot isolation a
    // transaction that then deletes a row another transaction touched after
    // that snapshot fails ("Record has changed since last read"); the
    // callers go on to delete the account's in-flight login states, which
    // other requests insert and delete all the time.
    let recorded: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT identity_pubkey FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut **tx)
            .await?;
    let Some(current_key) = recorded
        .flatten()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
    else {
        return Ok(());
    };
    let now = now_ms();
    let expires_at_ms = now.saturating_add(window_ms);

    let existing: Option<(u64, i64)> = sqlx::query_as(
        "SELECT seq, expires_at_ms FROM recall_anchors \
         WHERE account_id = ? AND identity_pubkey = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(&current_key[..])
    .fetch_optional(&mut **tx)
    .await?;
    match existing {
        Some((seq, expiry)) if expiry > now => {
            // The account as it was before the FIRST change under this key is
            // already on record. Its contents are never touched again.
            if change == Change::IdentityKey {
                sqlx::query(
                    "UPDATE recall_anchors SET expires_at_ms = GREATEST(expires_at_ms, ?) \
                     WHERE account_id = ? AND seq = ?",
                )
                .bind(expires_at_ms)
                .bind(&account_id[..])
                .bind(seq)
                .execute(&mut **tx)
                .await?;
            }
            return Ok(());
        },
        Some((seq, _)) => {
            // Expired and not yet pruned: it is dead, and a new first change
            // under this key gets a new anchor.
            sqlx::query("DELETE FROM recall_anchors WHERE account_id = ? AND seq = ?")
                .bind(&account_id[..])
                .bind(seq)
                .execute(&mut **tx)
                .await?;
        },
        None => {},
    }

    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM recall_anchors WHERE account_id = ? AND expires_at_ms > ? \
         FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(live).unwrap_or(usize::MAX) >= MAX_ANCHORS {
        tracing::warn!("recall: anchor cap reached; this change captures none");
        return Ok(());
    }

    let password: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT registration_bytes FROM opaque_registrations \
         WHERE account_id = ? AND kind = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(KIND_PASSWORD)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(password) = password else {
        return Ok(());
    };

    // ADR-0075 section 8: the recovery envelope in force now (what the
    // phrase arm is checked against) and whether the account has an email
    // vault (with one, the phrase arm is refused).
    let recovery: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT registration_bytes FROM opaque_registrations \
         WHERE account_id = ? AND kind = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(KIND_RECOVERY)
    .fetch_optional(&mut **tx)
    .await?;
    let mail_protected: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM email_vaults WHERE account_id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut **tx)
            .await?;

    // Above every row the account still has, so a later standing never
    // outranks an earlier one.
    let seq: u64 = sqlx::query_scalar(
        "SELECT CAST(COALESCE(MAX(seq), 0) + 1 AS UNSIGNED) FROM recall_anchors \
         WHERE account_id = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO recall_anchors \
            (account_id, seq, identity_pubkey, password_registration, \
             recovery_registration, mail_protected, key_generation, \
             created_at_ms, expires_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&account_id[..])
    .bind(seq)
    .bind(&current_key[..])
    .bind(&password)
    .bind(recovery.as_deref())
    .bind(mail_protected.is_some())
    .bind(key_generation)
    .bind(now)
    .bind(expires_at_ms)
    .execute(&mut **tx)
    .await?;
    tracing::info!(seq, "recall: anchor captured");
    Ok(())
}

/// When the account's recall window closes: the latest expiry among its
/// unexpired anchors, or `None` when it has none. `DELETE /account` refuses
/// while this is `Some` (ADR-0074 section 1.3).
///
/// A locking read, for the reason [`capture_anchor`] gives: the caller goes
/// on to delete in the same transaction.
///
/// # Errors
///
/// Any `sqlx::Error` from the read.
pub(crate) async fn open_window_until(
    tx: &mut Transaction<'_, MySql>,
    account_id: &[u8; 16],
    now: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(expires_at_ms) FROM recall_anchors \
         WHERE account_id = ? AND expires_at_ms > ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}

/// Hourly pruner: expired anchors, and recall states past their lifetime.
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = now_ms();
            match sqlx::query("DELETE FROM recall_anchors WHERE expires_at_ms <= ?")
                .bind(now)
                .execute(&pool)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired recall anchors");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "recall_anchors prune failed"),
            }
            if let Err(e) = sqlx::query("DELETE FROM recall_states WHERE created_at_ms < ?")
                .bind(now.saturating_sub(RECALL_STATE_TTL_MS))
                .execute(&pool)
                .await
            {
                tracing::warn!(error = %e, "recall_states prune failed");
            }
            if let Err(e) = sqlx::query("DELETE FROM recall_outcomes WHERE expires_at_ms <= ?")
                .bind(now)
                .execute(&pool)
                .await
            {
                tracing::warn!(error = %e, "recall_outcomes prune failed");
            }
        }
    })
}

// ----------------------------------------------------------------
// Shared: the anchor row and the statement check
// ----------------------------------------------------------------

/// One live anchor as the routes read it.
#[derive(Clone)]
struct Anchor {
    seq: u64,
    identity_pubkey: [u8; 32],
    password_registration: Vec<u8>,
    /// The recovery envelope in force at capture (ADR-0075 section 8).
    recovery_registration: Option<Vec<u8>>,
    /// The account had an email vault at capture: no phrase arm.
    mail_protected: bool,
    created_at_ms: i64,
    expires_at_ms: i64,
    attempts: u32,
}

impl Anchor {
    /// The envelope the phrase arm logs in against, when the arm exists for
    /// this anchor.
    fn phrase_envelope(&self) -> Option<&[u8]> {
        if self.mail_protected {
            None
        } else {
            self.recovery_registration.as_deref()
        }
    }
}

type AnchorRow = (u64, Vec<u8>, Vec<u8>, Option<Vec<u8>>, bool, i64, i64, u32);

fn anchor_from_row(row: AnchorRow) -> Option<Anchor> {
    let (
        seq,
        key,
        password_registration,
        recovery_registration,
        mail_protected,
        created_at_ms,
        expires_at_ms,
        attempts,
    ) = row;
    Some(Anchor {
        seq,
        identity_pubkey: <[u8; 32]>::try_from(key.as_slice()).ok()?,
        password_registration,
        recovery_registration,
        mail_protected,
        created_at_ms,
        expires_at_ms,
        attempts,
    })
}

/// The unexpired anchor `(account_id, seq)`, if any.
async fn live_anchor(
    pool: &MySqlPool,
    account_id: &[u8; 16],
    seq: u64,
    now: i64,
) -> Result<Option<Anchor>, sqlx::Error> {
    let row: Option<AnchorRow> = sqlx::query_as(
        "SELECT seq, identity_pubkey, password_registration, recovery_registration, \
                mail_protected, created_at_ms, expires_at_ms, attempts \
         FROM recall_anchors WHERE account_id = ? AND seq = ? AND expires_at_ms > ?",
    )
    .bind(&account_id[..])
    .bind(seq)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(anchor_from_row))
}

/// A key nobody holds a statement for: what a request is verified under when
/// there is no anchor, so that "no anchor" costs the same signature check as
/// "an anchor, and a bad signature". Whatever that check says is discarded.
fn stand_in_key() -> &'static VerifyingKey {
    static KEY: OnceLock<VerifyingKey> = OnceLock::new();
    KEY.get_or_init(|| {
        SigningKey::from_seed(&lakko_crypto::hash::blake3(
            b"lakko/auth/recall/stand-in-key/v1",
        ))
        .verifying_key()
    })
}

/// What a recall statement proves about its sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatementVerdict {
    /// A `kind` statement for the account over the expected body, inside the
    /// time window, signed by the anchor's key.
    Verified,
    /// The same, but for the time: signed by the anchor's key with a clock
    /// outside the window. The sender holds the key and is told the server's
    /// time (the module doc).
    Stale,
    /// Anything else, a missing anchor included.
    Refused,
}

/// Judge `statement` against `anchor`'s key. With no anchor the same work is
/// done against [`stand_in_key`] and the answer is `Refused`: exactly one
/// signature check either way, on the in-window path and on the stale one.
fn statement_verdict(
    statement: &[u8],
    anchor: Option<&Anchor>,
    kind: AccountStatementKind,
    account_id: &[u8; 16],
    body: &[u8],
    now: i64,
) -> StatementVerdict {
    let anchor_key = anchor.and_then(|a| VerifyingKey::from_bytes(&a.identity_pubkey).ok());
    let key = anchor_key.as_ref().unwrap_or_else(|| stand_in_key());
    let outcome = match verify_account_statement(statement, key, kind, account_id, body, now) {
        Ok(_) => StatementVerdict::Verified,
        // The window is checked before the signature, so this path has done
        // no signature check yet; the stale test does the one.
        Err(AccountStatementError::OutsideWindow)
            if account_statement_is_stale(statement, key, kind, account_id, body, now) =>
        {
            StatementVerdict::Stale
        },
        Err(e) => {
            tracing::debug!(error = %e, "recall: account statement refused");
            StatementVerdict::Refused
        },
    };
    if anchor_key.is_some() {
        outcome
    } else {
        StatementVerdict::Refused
    }
}

/// The refusal a verdict that is not `Verified` maps to.
fn refusal_of(verdict: StatementVerdict, now: i64) -> ApiError {
    match verdict {
        StatementVerdict::Stale => ApiError::StatementStale {
            server_time_ms: now,
        },
        StatementVerdict::Verified | StatementVerdict::Refused => ApiError::RecallRefused,
    }
}

/// Decode a `statement_b64` field. Anything that is not base64url is the
/// uniform refusal, not a `400`: the statement is the proof, and a bad proof
/// has one answer.
fn decode_statement(b64: &str) -> Result<Vec<u8>, ApiError> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ApiError::RecallRefused)
}

/// What a `recall-status` statement carries in its account field when the
/// device names the account by login handle (ADR-0075 section 8): the first
/// 16 bytes of the handle. The device cannot know the account id, and the
/// handle is a BLAKE3 output, so the value collides with a real account id
/// with probability 2^-128; the statement authorises a read only.
fn handle_statement_account(handle: &[u8; crate::login_handle::HANDLE_LEN]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&handle[..16]);
    out
}

/// Decode an `account_id_b64` field: URL-safe base64, no padding, 16 bytes.
fn decode_account_id(b64: &str) -> Result<[u8; 16], ApiError> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes.as_slice()).ok())
        .ok_or(ApiError::BadInput(
            "account_id_b64 must be 16 bytes of URL-safe base64",
        ))
}

fn opaque_internal<E: std::fmt::Debug>(what: &'static str) -> impl FnOnce(E) -> OpaqueError {
    move |e| {
        tracing::error!(error = ?e, what, "recall: internal opaque error");
        OpaqueError::Api(ApiError::Internal)
    }
}

// ----------------------------------------------------------------
// POST /account/recall/status
// ----------------------------------------------------------------

/// Body of `POST /account/recall/status`.
#[derive(Deserialize)]
pub(crate) struct StatusReq {
    /// URL-safe base64 of a 138-byte account statement: kind `recall-status`,
    /// the account, an empty body, signed by the OLD identity key.
    pub(crate) statement_b64: String,
    /// ADR-0075 section 8: the account's login handle (32 bytes, base64url
    /// without padding), sent by a device that holds the phrase and no
    /// cached account id. The statement then carries the FIRST 16 BYTES OF
    /// THE HANDLE in its account field, since the device cannot know the id;
    /// the answer carries the id.
    #[serde(default)]
    pub(crate) handle: Option<String>,
}

/// Answer of `POST /account/recall/status`.
#[derive(Serialize)]
pub(crate) struct StatusRes {
    /// The anchor the signing key may recall through; echoed in `start` and
    /// `finish`.
    pub(crate) anchor_seq: u64,
    /// Unix milliseconds at which the anchor expires.
    pub(crate) expires_at_ms: i64,
    /// Unix milliseconds of the first change made under the signing key: the
    /// password the recall asks for is the one from before this moment.
    pub(crate) changed_at_ms: i64,
    /// Password attempts the anchor still admits before it is destroyed.
    pub(crate) attempts_remaining: u32,
    /// URL-safe base64 of the 16-byte account id, for `start` and `finish`.
    /// What a device that named the account by handle did not have.
    pub(crate) account_id_b64: String,
    /// Whether `start` accepts `proof: "phrase"` for this anchor (ADR-0075
    /// section 8): the anchor holds a recovery envelope and the account had
    /// no email vault when it was captured.
    pub(crate) phrase_arm: bool,
}

/// `POST /account/recall/status` — anonymous, per-address limited.
///
/// The account is read from the statement itself. The answer is given only
/// to a statement that verifies under an unexpired anchor's key, so only a
/// device that holds an old identity key of the account learns anything; to
/// that device it says which anchor is its own, when the change happened,
/// until when it can act, and how many attempts are left. Everything else is
/// the uniform `401 recall-refused`.
///
/// The statement authorises a read and carries no server nonce: inside its
/// two-minute window it can be replayed to read the same answer again.
///
/// Whatever the account has, the handler performs exactly [`MAX_ANCHORS`]
/// signature checks (the missing ones against a stand-in key), so the time
/// taken does not say how many anchors exist, or whether any does.
///
/// A statement signed by an anchor's key with a clock outside the window is
/// `401 statement-stale` with the server's time (the module doc).
///
/// # Errors
///
/// [`ApiError::RecallRefused`], [`ApiError::StatementStale`], [`ApiError::Db`].
#[axum::debug_handler]
pub(crate) async fn status(
    State(state): State<AppState>,
    Json(req): Json<StatusReq>,
) -> Result<Json<StatusRes>, ApiError> {
    let statement = decode_statement(&req.statement_b64)?;
    let claims =
        AccountStatement::decode_unverified(&statement).map_err(|_| ApiError::RecallRefused)?;
    let now = now_ms();
    // By account id (the statement names it), or by login handle: then the
    // account is the one the handle is bound to (a stand-in with no anchors
    // for an unknown name) and the statement must name the handle's first 16
    // bytes.
    let (account_id, stated_account) = match req.handle.as_deref() {
        None => (claims.account_id, claims.account_id),
        Some(handle_b64) => {
            let pepper = crate::login_handle::pepper(&state)?;
            let handle = crate::login_handle::decode_handle(handle_b64)?;
            let resolved = crate::login_handle::resolve(&state.db, &pepper, &handle).await?;
            (resolved.account_id, handle_statement_account(&handle))
        },
    };

    let rows: Vec<AnchorRow> = sqlx::query_as(
        "SELECT seq, identity_pubkey, password_registration, recovery_registration, \
                mail_protected, created_at_ms, expires_at_ms, attempts \
         FROM recall_anchors WHERE account_id = ? AND expires_at_ms > ? ORDER BY seq LIMIT ?",
    )
    .bind(&account_id[..])
    .bind(now)
    .bind(i64::try_from(MAX_ANCHORS).unwrap_or(i64::MAX))
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::Db)?;
    let anchors: Vec<Anchor> = rows.into_iter().filter_map(anchor_from_row).collect();

    let mut found: Option<&Anchor> = None;
    let mut wrong_clock = false;
    for slot in 0..MAX_ANCHORS {
        let anchor = anchors.get(slot);
        match statement_verdict(
            &statement,
            anchor,
            AccountStatementKind::RecallStatus,
            &stated_account,
            &[],
            now,
        ) {
            StatementVerdict::Verified if found.is_none() => found = anchor,
            StatementVerdict::Stale => wrong_clock = true,
            StatementVerdict::Verified | StatementVerdict::Refused => {},
        }
    }
    let Some(anchor) = found else {
        // Signed by an anchor's key with a wrong clock: the device is told
        // the time, not that it has no anchor.
        return Err(if wrong_clock {
            ApiError::StatementStale {
                server_time_ms: now,
            }
        } else {
            ApiError::RecallRefused
        });
    };
    Ok(Json(StatusRes {
        anchor_seq: anchor.seq,
        expires_at_ms: anchor.expires_at_ms,
        changed_at_ms: anchor.created_at_ms,
        attempts_remaining: MAX_ATTEMPTS.saturating_sub(anchor.attempts),
        account_id_b64: URL_SAFE_NO_PAD.encode(account_id),
        phrase_arm: anchor.phrase_envelope().is_some(),
    }))
}

// ----------------------------------------------------------------
// POST /account/recall/start
// ----------------------------------------------------------------

/// Body of `POST /account/recall/start`.
#[derive(Deserialize)]
pub(crate) struct StartReq {
    /// URL-safe base64 of the 16-byte account id.
    pub(crate) account_id_b64: String,
    /// The anchor `status` named.
    pub(crate) anchor_seq: u64,
    /// URL-safe base64 of a 138-byte account statement: kind `recall-start`,
    /// the account, body `anchor_seq(8 BE) ‖ BLAKE3(login_request)`, signed
    /// by the OLD identity key.
    pub(crate) statement_b64: String,
    /// Standard base64 of the OPAQUE `CredentialRequest` made from the
    /// password as it was before the change (or, with `proof: "phrase"`,
    /// from the 64-byte seed of the recovery phrase as it was then).
    pub(crate) login_request: String,
    /// Which anchored envelope the login is against: `"password"` (the
    /// default) or `"phrase"` (ADR-0075 section 8).
    #[serde(default)]
    pub(crate) proof: Option<String>,
    /// Standard base64 of the OPAQUE `RegistrationRequest` for the NEW
    /// password.
    pub(crate) password_request: String,
    /// Standard base64 of the OPAQUE `RegistrationRequest` made from the NEW
    /// recovery phrase's 64-byte seed.
    pub(crate) recovery_request: String,
}

/// Answer of `POST /account/recall/start`.
#[derive(Serialize)]
pub(crate) struct StartRes {
    /// Names this attempt in `finish`. Single-use, valid ten minutes.
    pub(crate) session_id: String,
    /// Standard base64 of the OPAQUE `CredentialResponse`.
    pub(crate) login_response: String,
    /// Standard base64 of the `RegistrationResponse` for the new password.
    pub(crate) password_response: String,
    /// Standard base64 of the `RegistrationResponse` for the new recovery
    /// credential.
    pub(crate) recovery_response: String,
    /// URL-safe base64 of the 32-byte nonce the finish's statement covers.
    pub(crate) server_nonce_b64: String,
}

/// `POST /account/recall/start` — anonymous, per-address limited. One
/// password attempt.
///
/// Order:
///
/// 1. the body's shape (`400` for an account id or an OPAQUE message that
///    does not decode: that says nothing about any account);
/// 2. the anchor `(account, anchor_seq)` exists and is unexpired, and the
///    statement verifies under its key over this very login request: else
///    `401 recall-refused`, with nothing counted (or `401 statement-stale`
///    when only its time is wrong, also with nothing counted);
/// 3. [`Scope::Recall`] for this anchor (`429 too-many-attempts`);
/// 4. the anchor's attempt count, raised by one; an anchor already at
///    [`MAX_ATTEMPTS`] is destroyed and the answer is `401 recall-refused`;
/// 5. the OPAQUE login start against the anchored envelope, the two
///    registration starts, the stored state.
///
/// # Errors
///
/// [`ApiError::BadInput`], [`ApiError::RecallRefused`], [`ApiError::Limit`],
/// [`ApiError::Db`], [`ApiError::Internal`], inside [`OpaqueError`].
#[axum::debug_handler]
#[allow(clippy::too_many_lines)] // the order of the checks is the contract; kept in one place
pub(crate) async fn start(
    State(state): State<AppState>,
    Json(req): Json<StartReq>,
) -> Result<Json<StartRes>, OpaqueError> {
    let account_id = decode_account_id(&req.account_id_b64)?;
    let login_request_bytes = B64.decode(&req.login_request).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid login_request base64: {e}"),
        )
    })?;
    let login_request = CredentialRequest::<LakkoCipherSuite>::deserialize(&login_request_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid login_request: {e:?}"),
            )
        })?;
    let password_request = decode_registration_request(&req.password_request)?;
    let recovery_request = decode_registration_request(&req.recovery_request)?;
    let statement = decode_statement(&req.statement_b64)?;
    let phrase_arm = match req.proof.as_deref() {
        None | Some("password") => false,
        Some("phrase") => true,
        Some(_) => return Err(ApiError::BadInput("proof must be password or phrase").into()),
    };

    let now = now_ms();
    let anchor = live_anchor(&state.db, &account_id, req.anchor_seq, now)
        .await
        .map_err(ApiError::Db)?;
    let verdict = statement_verdict(
        &statement,
        anchor.as_ref(),
        AccountStatementKind::RecallStart,
        &account_id,
        &recall_start_body(req.anchor_seq, &login_request_bytes),
        now,
    );
    if verdict != StatementVerdict::Verified {
        return Err(refusal_of(verdict, now).into());
    }
    let anchor = anchor.ok_or(ApiError::RecallRefused)?;

    // From here on the caller holds the anchor's key.
    // The envelope this attempt is against. The phrase arm does not exist
    // for an account that had an email vault at capture (or an anchor from
    // before the recovery envelope was kept): the uniform refusal, with
    // nothing counted.
    let (anchored_envelope, anchored_kind) = if phrase_arm {
        let envelope = anchor.phrase_envelope().ok_or(ApiError::RecallRefused)?;
        (envelope.to_vec(), KIND_RECOVERY)
    } else {
        (anchor.password_registration.clone(), KIND_PASSWORD)
    };
    let mut limit_name = [0u8; 24];
    limit_name[..16].copy_from_slice(&account_id);
    limit_name[16..].copy_from_slice(&anchor.seq.to_be_bytes());
    crate::limits::charge_name(&state.db, &state.limits, Scope::Recall, &limit_name).await?;

    let counted = sqlx::query(
        "UPDATE recall_anchors SET attempts = attempts + 1 \
         WHERE account_id = ? AND seq = ? AND attempts < ? AND expires_at_ms > ?",
    )
    .bind(&account_id[..])
    .bind(anchor.seq)
    .bind(MAX_ATTEMPTS)
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?
    .rows_affected();
    if counted != 1 {
        // Spent (or gone meanwhile). A spent anchor is destroyed here.
        sqlx::query(
            "DELETE FROM recall_anchors WHERE account_id = ? AND seq = ? AND attempts >= ?",
        )
        .bind(&account_id[..])
        .bind(anchor.seq)
        .bind(MAX_ATTEMPTS)
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?;
        tracing::info!(
            seq = anchor.seq,
            "recall: anchor spent its attempts; destroyed"
        );
        return Err(ApiError::RecallRefused.into());
    }

    // The login against the envelope the anchor holds. The credential
    // identifier is the kind's the envelope was registered under.
    let registration = ServerRegistration::<LakkoCipherSuite>::deserialize(&anchored_envelope)
        .map_err(opaque_internal(
            "anchored registration does not deserialize",
        ))?;
    let anchored_credential_id = build_credential_id(&account_id, anchored_kind);
    let login_start = ServerLogin::<LakkoCipherSuite>::start(
        &mut OsRng,
        &state.opaque_setup,
        Some(registration),
        login_request,
        &anchored_credential_id,
        ServerLoginStartParameters::default(),
    )
    .map_err(opaque_internal("ServerLogin::start"))?;

    let password_response = opaque_registration_start(
        &state.opaque_setup,
        &account_id,
        KIND_PASSWORD,
        password_request,
    )?;
    let recovery_response = opaque_registration_start(
        &state.opaque_setup,
        &account_id,
        KIND_RECOVERY,
        recovery_request,
    )?;

    let mut session_id = [0u8; 16];
    OsRng.fill_bytes(&mut session_id);
    let mut server_nonce = [0u8; 32];
    OsRng.fill_bytes(&mut server_nonce);
    sqlx::query(
        "INSERT INTO recall_states \
            (session_id, account_id, anchor_seq, server_nonce, state_bytes, created_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id[..])
    .bind(&account_id[..])
    .bind(anchor.seq)
    .bind(&server_nonce[..])
    .bind(login_start.state.serialize().to_vec())
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(ApiError::Db)?;

    Ok(Json(StartRes {
        session_id: Uuid::from_bytes(session_id).to_string(),
        login_response: B64.encode(login_start.message.serialize()),
        password_response,
        recovery_response,
        server_nonce_b64: URL_SAFE_NO_PAD.encode(server_nonce),
    }))
}

// ----------------------------------------------------------------
// POST /account/recall/finish
// ----------------------------------------------------------------

/// Body of `POST /account/recall/finish`.
#[derive(Deserialize)]
pub(crate) struct FinishReq {
    /// The `session_id` the start returned.
    pub(crate) session_id: String,
    /// URL-safe base64 of the 16-byte account id.
    pub(crate) account_id_b64: String,
    /// The anchor the start was made for.
    pub(crate) anchor_seq: u64,
    /// Standard base64 of the OPAQUE `CredentialFinalization` of the login
    /// against the anchored password.
    pub(crate) login_finish: String,
    /// Standard base64 of the `RegistrationUpload` for the NEW password.
    pub(crate) password_upload: String,
    /// Standard base64 of the `RegistrationUpload` for the NEW recovery seed.
    pub(crate) recovery_upload: String,
    /// URL-safe base64 of the NEW identity's 32-byte Ed25519 signature
    /// public key. Not the anchor's key: the old identity does not return.
    pub(crate) new_signature_pubkey_b64: String,
    /// URL-safe base64 of the 32-byte device id the new pair names. Absent:
    /// the pair names no device and the client binds one afterwards.
    #[serde(default)]
    pub(crate) device_id_b64: Option<String>,
    /// URL-safe base64 of a 138-byte account statement: kind `recall`, the
    /// account, body `server_nonce ‖ new_signature_pubkey ‖
    /// BLAKE3(password_upload) ‖ BLAKE3(recovery_upload)`, signed by the OLD
    /// identity key.
    pub(crate) statement_b64: String,
}

/// Answer of `POST /account/recall/finish`: the key reset's answer plus a
/// fresh-auth grant.
#[derive(Serialize)]
pub(crate) struct FinishRes {
    /// URL-safe base64 of the 177-byte key-reset attestation the client
    /// carries to discovery and to the DS, exactly as after a reset.
    pub(crate) key_reset_attest_b64: String,
    /// `key_generation`, `session_token`, `session_expires_at_ms`,
    /// `refresh_token`, `refresh_expires_at_ms`, at the new generation.
    #[serde(flatten)]
    pub(crate) session: SessionPair,
    /// `fresh_grant`, `fresh_grant_expires_at_ms`: the person proved the
    /// anchor key and the password in this exchange.
    #[serde(flatten)]
    pub(crate) grant: FreshGrant,
}

/// The in-flight state of one attempt, as `finish` reads it.
struct RecallState {
    account_id: [u8; 16],
    anchor_seq: u64,
    server_nonce: [u8; 32],
    state_bytes: Vec<u8>,
    created_at_ms: i64,
}

/// `(account_id, anchor_seq, server_nonce, state_bytes, created_at_ms)` of a
/// `recall_states` row.
type StateRow = (Vec<u8>, u64, Vec<u8>, Vec<u8>, i64);

async fn load_state(
    pool: &MySqlPool,
    session_id: &[u8; 16],
    now: i64,
) -> Result<Option<RecallState>, sqlx::Error> {
    let row: Option<StateRow> = sqlx::query_as(
        "SELECT account_id, anchor_seq, server_nonce, state_bytes, created_at_ms \
         FROM recall_states WHERE session_id = ? AND created_at_ms > ?",
    )
    .bind(&session_id[..])
    .bind(now.saturating_sub(RECALL_STATE_TTL_MS))
    .fetch_optional(pool)
    .await?;
    Ok(
        row.and_then(|(account, anchor_seq, nonce, state_bytes, created_at_ms)| {
            Some(RecallState {
                account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
                anchor_seq,
                server_nonce: <[u8; 32]>::try_from(nonce.as_slice()).ok()?,
                state_bytes,
                created_at_ms,
            })
        }),
    )
}

/// A finish that already succeeded, as a retry reads it.
struct RecallOutcome {
    account_id: [u8; 16],
    request_digest: [u8; 32],
    key_generation: u64,
    device_id: crate::session::DeviceId,
    attestation: Vec<u8>,
}

/// `(account_id, request_digest, key_generation, device_id, attestation)` of
/// a `recall_outcomes` row.
type OutcomeRow = (Vec<u8>, Vec<u8>, u64, Vec<u8>, Vec<u8>);

/// The unexpired outcome recorded for the attempt `session_id`, if its
/// finish already succeeded.
async fn load_outcome(
    pool: &MySqlPool,
    session_id: &[u8; 16],
    now: i64,
) -> Result<Option<RecallOutcome>, sqlx::Error> {
    let row: Option<OutcomeRow> = sqlx::query_as(
        "SELECT account_id, request_digest, key_generation, device_id, attestation \
         FROM recall_outcomes WHERE session_id = ? AND expires_at_ms > ?",
    )
    .bind(&session_id[..])
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.and_then(|(account, digest, key_generation, device, attestation)| {
            Some(RecallOutcome {
                account_id: <[u8; 16]>::try_from(account.as_slice()).ok()?,
                request_digest: <[u8; 32]>::try_from(digest.as_slice()).ok()?,
                key_generation,
                device_id: <[u8; 32]>::try_from(device.as_slice()).ok()?,
                attestation,
            })
        }),
    )
}

/// Domain of [`request_digest`].
const REQUEST_DIGEST_DOMAIN: &[u8] = b"lakko/auth/recall/finish-request/v1";

/// BLAKE3 over every field of a finish request, in a fixed-width layout (the
/// variable-length fields enter as their own BLAKE3), so two requests have
/// one digest only when they are the same request. What a retry is compared
/// by: the statement inside it is signed by the anchor's key over the
/// server nonce, the new key and both uploads, so nobody can build a request
/// with this digest without having held that key during the attempt.
fn request_digest(input: &FinishInput, anchor_seq: u64) -> [u8; 32] {
    use lakko_crypto::hash::blake3;
    let mut buf = Vec::with_capacity(REQUEST_DIGEST_DOMAIN.len() + 1 + 16 + 16 + 8 + 32 * 6);
    buf.extend_from_slice(REQUEST_DIGEST_DOMAIN);
    buf.push(0);
    buf.extend_from_slice(&input.session_id);
    buf.extend_from_slice(&input.account_id);
    buf.extend_from_slice(&anchor_seq.to_be_bytes());
    buf.extend_from_slice(&input.device_id);
    buf.extend_from_slice(&input.new_signature_pubkey);
    buf.extend_from_slice(&blake3(&input.login_finish_bytes));
    buf.extend_from_slice(&blake3(&input.password_upload_bytes));
    buf.extend_from_slice(&blake3(&input.recovery_upload_bytes));
    buf.extend_from_slice(&blake3(&input.statement));
    blake3(&buf)
}

/// Compare two digests without an early exit.
fn digests_equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Answer a retried finish from its recorded outcome (the module doc): the
/// request must be the very one that succeeded and the account must still be
/// at the generation that recall set. Issues a pair and a fresh-auth grant at
/// that generation and returns the recorded attestation. Changes nothing.
///
/// # Errors
///
/// [`ApiError::RecallRefused`] for a different request or an account that
/// has moved on; [`ApiError::Db`]; [`ApiError::Internal`].
async fn replay_outcome(
    state: &AppState,
    outcome: &RecallOutcome,
    input: &FinishInput,
    anchor_seq: u64,
) -> Result<FinishRes, ApiError> {
    if outcome.account_id != input.account_id
        || outcome.device_id != input.device_id
        || !digests_equal(&outcome.request_digest, &request_digest(input, anchor_seq))
    {
        return Err(ApiError::RecallRefused);
    }
    let current = crate::session::key_generation_of(&state.db, &input.account_id)
        .await
        .map_err(ApiError::Db)?;
    if current != Some(outcome.key_generation) {
        return Err(ApiError::RecallRefused);
    }
    let session = crate::session::issue_pair_at(
        state,
        &input.account_id,
        outcome.key_generation,
        &outcome.device_id,
    )?;
    let grant =
        crate::session::issue_fresh_grant(state, &input.account_id, outcome.key_generation)?;
    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(input.account_id),
        key_generation = outcome.key_generation,
        "recall finish answered again from its recorded outcome",
    );
    Ok(FinishRes {
        key_reset_attest_b64: URL_SAFE_NO_PAD.encode(&outcome.attestation),
        session,
        grant,
    })
}

/// What `finish` decoded from its body before it looked anything up.
struct FinishInput {
    session_id: [u8; 16],
    account_id: [u8; 16],
    device_id: crate::session::DeviceId,
    login_finish: CredentialFinalization<LakkoCipherSuite>,
    login_finish_bytes: Vec<u8>,
    password_upload_bytes: Vec<u8>,
    recovery_upload_bytes: Vec<u8>,
    password_registration: Vec<u8>,
    recovery_registration: Vec<u8>,
    new_signature_pubkey: [u8; 32],
    statement: Vec<u8>,
}

fn decode_finish(req: &FinishReq) -> Result<FinishInput, OpaqueError> {
    let session_id = *Uuid::parse_str(&req.session_id)
        .map_err(|_| ApiError::BadInput("session_id is not a UUID"))?
        .as_bytes();
    let account_id = decode_account_id(&req.account_id_b64)?;
    let device_id = crate::session::decode_device_id(req.device_id_b64.as_deref())?;
    let finalize_bytes = B64.decode(&req.login_finish).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid login_finish base64: {e}"),
        )
    })?;
    let login_finish = CredentialFinalization::<LakkoCipherSuite>::deserialize(&finalize_bytes)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid login_finish: {e:?}"),
            )
        })?;
    // The statement covers the uploads as the raw OPAQUE bytes, so those are
    // kept beside the registrations made from them.
    let raw = |field: &'static str, b64: &str| {
        B64.decode(b64).map_err(|e| {
            OpaqueError::Plain(
                StatusCode::BAD_REQUEST,
                format!("invalid {field} base64: {e}"),
            )
        })
    };
    let password_upload_bytes = raw("password_upload", &req.password_upload)?;
    let recovery_upload_bytes = raw("recovery_upload", &req.recovery_upload)?;
    let password_registration = ServerRegistration::<LakkoCipherSuite>::finish(
        decode_registration_upload(&req.password_upload)?,
    )
    .serialize()
    .to_vec();
    let recovery_registration = ServerRegistration::<LakkoCipherSuite>::finish(
        decode_registration_upload(&req.recovery_upload)?,
    )
    .serialize()
    .to_vec();
    let new_signature_pubkey: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&req.new_signature_pubkey_b64)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or(ApiError::BadInput(
            "new_signature_pubkey_b64 must be 32 bytes of URL-safe base64",
        ))?;
    crate::genesis_attest::require_usable_identity_key(&new_signature_pubkey)?;
    Ok(FinishInput {
        session_id,
        account_id,
        device_id,
        login_finish,
        login_finish_bytes: finalize_bytes,
        password_upload_bytes,
        recovery_upload_bytes,
        password_registration,
        recovery_registration,
        new_signature_pubkey,
        statement: decode_statement(&req.statement_b64)?,
    })
}

/// `POST /account/recall/finish` — anonymous, per-address limited.
///
/// Order:
///
/// 1. the body's shape (`400`); then, if this attempt's finish ALREADY
///    succeeded, the retry rule of the module doc: the identical request is
///    answered again, any other is `401 recall-refused`;
/// 2. the attempt's state exists, is unexpired and is for this account and
///    anchor; the anchor exists and is unexpired; the statement verifies
///    under the anchor's key over the state's server nonce, the new key and
///    both uploads. Any failure: `401 recall-refused`, nothing consumed
///    (`401 statement-stale` when the statement is right but for its time);
/// 3. the state is consumed (a second finish for it is `recall-refused`);
/// 4. the OPAQUE login finish against the anchored password: `401
///    wrong-password` (the attempt was counted when it was opened);
/// 5. one transaction, the key reset's with these differences: no
///    fresh-auth grant is asked for; the anchor is locked and it and every
///    anchor above it are deleted, the ones below it are kept; NO anchor is
///    captured; the password envelope is replaced as well as the recovery
///    credential; the TOTP secret is deleted; every in-flight login, reset
///    and recall state of the account is deleted; every device session floor
///    is dropped. The login handle is kept.
///
/// After the commit every credential of the previous generation is refused,
/// the displaced party's password, recovery phrase, TOTP secret and passkeys
/// are gone, and the account's identity key is the new one. The answer
/// carries the key-reset attestation discovery and the DS accept as they
/// accept a reset's, a session pair and a fresh-auth grant.
///
/// # Errors
///
/// [`ApiError::BadInput`], [`ApiError::RecallRefused`],
/// [`ApiError::StatementStale`], [`ApiError::WrongPassword`],
/// [`ApiError::Db`], [`ApiError::Internal`], inside [`OpaqueError`].
#[axum::debug_handler]
#[allow(clippy::too_many_lines)] // one transaction, read top to bottom like the key reset's
pub(crate) async fn finish(
    State(state): State<AppState>,
    Json(req): Json<FinishReq>,
) -> Result<Json<FinishRes>, OpaqueError> {
    let input = decode_finish(&req)?;
    let account_id = input.account_id;
    let now = now_ms();

    // 1b. A finish for an attempt that already succeeded: the lost answer,
    //     asked again. Answered from the record or refused; it never reaches
    //     the steps below (the anchor and the state it would need are gone).
    if let Some(outcome) = load_outcome(&state.db, &input.session_id, now)
        .await
        .map_err(ApiError::Db)?
    {
        return Ok(Json(
            replay_outcome(&state, &outcome, &input, req.anchor_seq).await?,
        ));
    }

    // The digest a retry is compared by, taken while the request is whole.
    let digest = request_digest(&input, req.anchor_seq);

    // 2. The state, the anchor, the statement. With no state the statement
    //    is still checked, against a nonce nobody was given.
    let recall_state = load_state(&state.db, &input.session_id, now)
        .await
        .map_err(ApiError::Db)?
        .filter(|s| s.account_id == account_id && s.anchor_seq == req.anchor_seq);
    let anchor = live_anchor(&state.db, &account_id, req.anchor_seq, now)
        .await
        .map_err(ApiError::Db)?;
    let server_nonce = recall_state.as_ref().map_or([0u8; 32], |s| s.server_nonce);
    let verdict = statement_verdict(
        &input.statement,
        anchor.as_ref(),
        AccountStatementKind::Recall,
        &account_id,
        &recall_body(
            &server_nonce,
            &input.new_signature_pubkey,
            &input.password_upload_bytes,
            &input.recovery_upload_bytes,
        ),
        now,
    );
    let (Some(recall_state), Some(anchor), StatementVerdict::Verified) =
        (recall_state, anchor, verdict)
    else {
        // A stale statement is told the time only when it is otherwise the
        // statement of this very attempt (it verified over the stored
        // nonce); nothing was consumed.
        return Err(refusal_of(verdict, now).into());
    };
    if input.new_signature_pubkey == anchor.identity_pubkey {
        // The old identity authorises the recall and does not return
        // (ADR-0074 D10, invariant #6).
        return Err(ApiError::BadInput("new_signature_pubkey must be a new key").into());
    }

    // 3. Consume the attempt. Of two finishes for one state, one deletes it.
    let consumed = sqlx::query("DELETE FROM recall_states WHERE session_id = ?")
        .bind(&input.session_id[..])
        .execute(&state.db)
        .await
        .map_err(ApiError::Db)?
        .rows_affected();
    if consumed != 1 {
        return Err(ApiError::RecallRefused.into());
    }

    // 4. The password as it was before the change.
    let server_login = ServerLogin::<LakkoCipherSuite>::deserialize(&recall_state.state_bytes)
        .map_err(opaque_internal("recall state does not deserialize"))?;
    if let Err(e) = server_login.finish(input.login_finish) {
        tracing::debug!(error = ?e, "recall: anchored password did not verify");
        return Err(ApiError::WrongPassword.into());
    }

    // 5. The transaction.
    let mut conn = crate::session::read_committed_conn(&state.db)
        .await
        .map_err(ApiError::Db)?;
    let mut tx = sqlx::Acquire::begin(&mut conn)
        .await
        .map_err(ApiError::Db)?;
    let locked: Option<u64> =
        sqlx::query_scalar("SELECT identity_version FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    if locked.is_none() {
        return Err(ApiError::RecallRefused.into());
    }
    //    The anchor again, under its lock: a recall through an older anchor
    //    that committed since the read above has deleted this one.
    let still: Option<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT identity_pubkey, expires_at_ms FROM recall_anchors \
         WHERE account_id = ? AND seq = ? FOR UPDATE",
    )
    .bind(&account_id[..])
    .bind(anchor.seq)
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    match still {
        Some((key, expiry)) if key == anchor.identity_pubkey && expiry > now => {},
        _ => return Err(ApiError::RecallRefused.into()),
    }
    //    This anchor and every newer standing; the older ones stand.
    sqlx::query("DELETE FROM recall_anchors WHERE account_id = ? AND seq >= ?")
        .bind(&account_id[..])
        .bind(anchor.seq)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;

    //    The generation and the identity key. No anchor is captured for the
    //    key this displaces.
    sqlx::query(
        "UPDATE accounts SET identity_version = LAST_INSERT_ID(identity_version + 1), \
         identity_pubkey = ? WHERE id = ?",
    )
    .bind(&input.new_signature_pubkey[..])
    .bind(&account_id[..])
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    let key_generation: u64 = sqlx::query_scalar("SELECT LAST_INSERT_ID()")
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::Db)?;

    //    Every in-flight proof of the account, of every kind: a password
    //    login started against the displaced password must not finish.
    for statement in [
        "DELETE FROM opaque_login_states WHERE account_id = ?",
        "DELETE FROM recall_states WHERE account_id = ?",
        "DELETE FROM totp_secrets WHERE account_id = ?",
        "DELETE FROM device_session_floors WHERE account_id = ?",
        // ADR-0075: the displaced party may have added a recovery email of
        // its own; a mailed code must not bring it back in. (The email vault
        // and the tickets go with `delete_superseded_state` below.)
        "DELETE FROM recovery_emails WHERE account_id = ?",
        "DELETE FROM email_challenges WHERE account_id = ?",
    ] {
        sqlx::query(statement)
            .bind(&account_id[..])
            .execute(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    }
    for (kind, bytes) in [
        (KIND_PASSWORD, &input.password_registration),
        (KIND_RECOVERY, &input.recovery_registration),
    ] {
        sqlx::query(
            "INSERT INTO opaque_registrations (account_id, kind, registration_bytes) \
             VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE registration_bytes = VALUES(registration_bytes)",
        )
        .bind(&account_id[..])
        .bind(kind)
        .bind(bytes)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Db)?;
    }
    crate::key_reset::delete_superseded_state(&mut tx, &account_id).await?;

    let attestation = KeyResetAttestation::for_account(
        &account_id,
        key_generation,
        input.new_signature_pubkey,
        now_ms(),
    )
    .sign(&state.attest_key.signing);
    let session =
        crate::session::issue_pair_at(&state, &account_id, key_generation, &input.device_id)?;
    let grant = crate::session::issue_fresh_grant(&state, &account_id, key_generation)?;
    //    The outcome, so that this very request can be answered again if its
    //    answer is lost (the module doc). In the same transaction: a recall
    //    that committed always has its record.
    sqlx::query(
        "INSERT INTO recall_outcomes \
            (session_id, account_id, request_digest, key_generation, device_id, attestation, \
             expires_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&input.session_id[..])
    .bind(&account_id[..])
    .bind(&digest[..])
    .bind(key_generation)
    .bind(&input.device_id[..])
    .bind(&attestation[..])
    .bind(
        recall_state
            .created_at_ms
            .saturating_add(RECALL_STATE_TTL_MS),
    )
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;

    tracing::info!(
        account_id = %URL_SAFE_NO_PAD.encode(account_id),
        key_generation,
        anchor_seq = anchor.seq,
        "account recalled",
    );
    Ok(Json(FinishRes {
        key_reset_attest_b64: URL_SAFE_NO_PAD.encode(attestation),
        session,
        grant,
    }))
}
