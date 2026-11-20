// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! One account per identity key, proven, with history ([ADR-0098] security
//! fix F3).
//!
//! Auth records an account's identity key (`accounts.identity_pubkey`) in
//! four places: the genesis mint, the key reset, the key rotation and the
//! recall. A moderator statement names an account by the key's target
//! (`lakko_moderation::target::account_target_hash`). Before this module the
//! key was client-asserted, so an account could record another person's
//! public key and be found as that person. Now every writer:
//!
//! 1. requires `identity_proof_b64`, the identity key's own signature over
//!    the account id and the key ([`lakko_attest::verify_identity_proof`]),
//!    else `400 {"error":"identity-proof-invalid"}`
//!    ([`require_identity_proof`]);
//! 2. claims the key's target for the account in
//!    `account_identity_targets` in the same transaction as the write
//!    ([`claim`]): a target belongs to one account forever, and another
//!    account asserting it gets `409 {"error":"identity-key-taken"}`.
//!
//! The history table holds every target an account ever had. Suspend, Lift,
//! KeepSuspension and the panel's account read find the account through it
//! ([`account_of_target`]), so a statement naming the key an account had
//! before a reset or a rotation still finds it.
//!
//! Deleting an account deletes its history rows in the same transaction
//! ([`on_account_deleted`]), except when the account is suspended at that
//! moment: then the rows are kept until the suspension's end (no end: kept
//! for good), so a suspended person cannot delete the account and register
//! the same key on a new one. A new account asserting a kept target gets
//! `409 identity-key-taken` while it is kept; a kept row whose end has
//! passed counts as free and is dropped by the claim that meets it and by
//! the hourly sweep ([`sweep_kept`]); a moderator's Lift naming a kept target
//! releases them at once ([`release_kept`]). What a kept row holds is the 32-byte
//! target, the 16-byte id of an account that no longer exists, and the end.
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use axum::http::StatusCode;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use sqlx::{MySql, MySqlConnection};

use crate::webauthn::ApiError;

/// The body code of a missing or invalid identity proof.
pub(crate) const CODE_IDENTITY_PROOF_INVALID: &str = "identity-proof-invalid";
/// The body code of a key whose target another account holds.
pub(crate) const CODE_IDENTITY_KEY_TAKEN: &str = "identity-key-taken";

/// The target of an identity key.
pub(crate) fn target_of(pubkey: &[u8; 32]) -> [u8; 32] {
    lakko_moderation::target::account_target_hash(pubkey)
}

/// Check `identity_proof_b64` (base64 in any of the four common spellings)
/// as `pubkey`'s identity proof for `account_id`: `400 identity-proof-invalid`
/// when it is absent, not base64, or does not verify.
///
/// # Errors
///
/// [`ApiError::Coded`].
pub(crate) fn require_identity_proof(
    identity_proof_b64: Option<&str>,
    account_id: &[u8; 16],
    pubkey: &[u8; 32],
) -> Result<(), ApiError> {
    let invalid = || ApiError::Coded(StatusCode::BAD_REQUEST, CODE_IDENTITY_PROOF_INVALID);
    let text = identity_proof_b64.map(str::trim).ok_or_else(invalid)?;
    let proof = URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| STANDARD.decode(text))
        .or_else(|_| URL_SAFE.decode(text))
        .or_else(|_| STANDARD_NO_PAD.decode(text))
        .map_err(|_| invalid())?;
    lakko_attest::verify_identity_proof(&proof, account_id, pubkey).map_err(|_| {
        tracing::info!("identity key refused: the identity proof does not verify");
        invalid()
    })
}

/// The account that holds `target`: a live account's row, or a row kept
/// after its account was deleted while suspended, until that suspension's
/// end. A kept row whose end is at or before `now_ms` holds nothing.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn owner_of<'e, E>(
    executor: E,
    target: &[u8; 32],
    now_ms: i64,
) -> Result<Option<[u8; 16]>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT account_id FROM account_identity_targets WHERE target = ? \
             AND (kept = 0 OR kept_until_ms IS NULL OR kept_until_ms > ?)",
    )
    .bind(&target[..])
    .bind(now_ms)
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok()))
}

/// What deleting `account_id` does to its history, inside the caller's
/// deleting transaction and before the account row (and with it the
/// suspension row) is deleted: when the account is suspended at `now_ms`,
/// its rows are marked kept until the suspension's end (`NULL` for no end);
/// otherwise they are deleted. Returns whether they were kept.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn on_account_deleted(
    conn: &mut MySqlConnection,
    account_id: &[u8],
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    let suspension: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT until_ms FROM account_suspensions \
         WHERE account_id = ? AND (until_ms IS NULL OR until_ms > ?) FOR UPDATE",
    )
    .bind(account_id)
    .bind(now_ms)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(until_ms) = suspension {
        // The newest moderator decision applied to the account goes with the
        // account (its row cascades); it is kept here, so a Lift that would
        // have been superseded on the live account is superseded here too.
        let decided_at: Option<u64> = sqlx::query_scalar(
            "SELECT issued_at_ms FROM moderation_account_decisions WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&mut *conn)
        .await?;
        sqlx::query(
            "UPDATE account_identity_targets \
             SET kept = 1, kept_until_ms = ?, kept_decided_at_ms = ? \
             WHERE account_id = ?",
        )
        .bind(until_ms)
        .bind(decided_at)
        .bind(account_id)
        .execute(&mut *conn)
        .await?;
        tracing::info!("deleted account was suspended: its identity keys stay taken");
        Ok(true)
    } else {
        sqlx::query("DELETE FROM account_identity_targets WHERE account_id = ?")
            .bind(account_id)
            .execute(&mut *conn)
            .await?;
        Ok(false)
    }
}

/// What a Lift did to the kept history of a deleted account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Release {
    /// The target's kept rows, and every other kept row of the same deleted
    /// account, were deleted: its keys are free.
    Released,
    /// The Lift was issued before the newest decision applied to the account
    /// before its deletion; nothing changed.
    Superseded,
    /// No kept row holds the target.
    NothingKept,
}

/// A Lift for `target` when no live account holds it, inside the apply
/// transaction: if a row kept after a suspended account's deletion holds
/// the target, and the Lift was issued no earlier than the newest decision
/// applied to that account before it was deleted, delete every kept row of
/// that account.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn release_kept(
    conn: &mut MySqlConnection,
    target: &[u8],
    issued_at_ms: u64,
) -> Result<Release, sqlx::Error> {
    let row: Option<(Vec<u8>, Option<u64>)> = sqlx::query_as(
        "SELECT account_id, kept_decided_at_ms FROM account_identity_targets \
         WHERE target = ? AND kept = 1 FOR UPDATE",
    )
    .bind(target)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((account_id, decided_at)) = row else {
        return Ok(Release::NothingKept);
    };
    if decided_at.is_some_and(|d| issued_at_ms < d) {
        return Ok(Release::Superseded);
    }
    sqlx::query("DELETE FROM account_identity_targets WHERE account_id = ? AND kept = 1")
        .bind(&account_id)
        .execute(&mut *conn)
        .await?;
    tracing::info!("a lift released the identity keys of a deleted account");
    Ok(Release::Released)
}

/// Drop the kept rows whose suspension ended at or before `now_ms`. Run by
/// the hourly suspension sweep. Returns how many were dropped.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn sweep_kept<'e, E>(executor: E, now_ms: i64) -> Result<u64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    sqlx::query(
        "DELETE FROM account_identity_targets \
         WHERE kept = 1 AND kept_until_ms IS NOT NULL AND kept_until_ms <= ?",
    )
    .bind(now_ms)
    .execute(executor)
    .await
    .map(|r| r.rows_affected())
}

/// Refuse a key whose target another account holds, without claiming it: the
/// pre-check of a route that consumes something before its transaction.
///
/// # Errors
///
/// [`ApiError::Coded`] (`409 identity-key-taken`), [`ApiError::Db`].
pub(crate) async fn require_free<'e, E>(
    executor: E,
    account_id: &[u8; 16],
    pubkey: &[u8; 32],
) -> Result<(), ApiError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    match owner_of(executor, &target_of(pubkey), crate::session::now_ms())
        .await
        .map_err(ApiError::Db)?
    {
        Some(owner) if &owner != account_id => {
            tracing::info!("identity key refused: another account holds it");
            Err(ApiError::Coded(
                StatusCode::CONFLICT,
                CODE_IDENTITY_KEY_TAKEN,
            ))
        },
        _ => Ok(()),
    }
}

/// Claim `pubkey`'s target for `account_id` inside the caller's transaction:
/// drop a kept row whose suspension ended, insert the history row unless one
/// exists, then read who holds it. Another account, or a row kept after a
/// suspended account's deletion, is `409 identity-key-taken` (the caller
/// rolls back); the same account (a key it had before) is fine. Two racing claims of one target
/// insert one row; the loser reads the winner.
///
/// # Errors
///
/// [`ApiError::Coded`] (`409 identity-key-taken`), [`ApiError::Db`].
pub(crate) async fn claim(
    conn: &mut MySqlConnection,
    account_id: &[u8; 16],
    pubkey: &[u8; 32],
    now_ms: i64,
) -> Result<(), ApiError> {
    let target = target_of(pubkey);
    // A row kept after a suspended account's deletion whose suspension has
    // ended holds nothing: drop it so the claim below can take the target.
    sqlx::query(
        "DELETE FROM account_identity_targets WHERE target = ? \
             AND kept = 1 AND kept_until_ms IS NOT NULL AND kept_until_ms <= ?",
    )
    .bind(&target[..])
    .bind(now_ms)
    .execute(&mut *conn)
    .await
    .map_err(ApiError::Db)?;
    sqlx::query(
        "INSERT IGNORE INTO account_identity_targets (target, account_id, added_at_ms) \
         VALUES (?, ?, ?)",
    )
    .bind(&target[..])
    .bind(&account_id[..])
    .bind(now_ms)
    .execute(&mut *conn)
    .await
    .map_err(ApiError::Db)?;
    let owner: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT account_id FROM account_identity_targets WHERE target = ? FOR UPDATE",
    )
    .bind(&target[..])
    .fetch_optional(&mut *conn)
    .await
    .map_err(ApiError::Db)?;
    if owner.as_deref() == Some(&account_id[..]) {
        Ok(())
    } else {
        tracing::info!("identity key refused: another account holds it");
        Err(ApiError::Coded(
            StatusCode::CONFLICT,
            CODE_IDENTITY_KEY_TAKEN,
        ))
    }
}

/// The live account that holds `target` (through the history, so a key the
/// account had before a change still finds it), locked for update when
/// `for_update`. `None` when no account ever held it or the account is gone.
///
/// # Errors
///
/// Any `sqlx::Error`.
pub(crate) async fn account_of_target<'e, E>(
    executor: E,
    target: &[u8],
    for_update: bool,
) -> Result<Option<Vec<u8>>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let sql = if for_update {
        "SELECT a.id FROM account_identity_targets t JOIN accounts a ON a.id = t.account_id \
         WHERE t.target = ? FOR UPDATE"
    } else {
        "SELECT a.id FROM account_identity_targets t JOIN accounts a ON a.id = t.account_id \
         WHERE t.target = ?"
    };
    sqlx::query_scalar(sql)
        .bind(target)
        .fetch_optional(executor)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proof_is_required_and_checked() {
        let seed = [7u8; 32];
        let pubkey = lakko_crypto::signing::SigningKey::from_seed(&seed)
            .verifying_key()
            .to_bytes();
        let account = [3u8; 16];
        let proof = lakko_attest::sign_identity_proof(&seed, &account);
        let code = |r: Result<(), ApiError>| match r {
            Ok(()) => "ok",
            Err(ApiError::Coded(StatusCode::BAD_REQUEST, c)) => c,
            Err(_) => "other",
        };
        for text in [
            URL_SAFE_NO_PAD.encode(proof),
            STANDARD.encode(proof),
            URL_SAFE.encode(proof),
        ] {
            assert_eq!(
                code(require_identity_proof(Some(&text), &account, &pubkey)),
                "ok"
            );
        }
        let encoded = URL_SAFE_NO_PAD.encode(proof);
        assert_eq!(
            code(require_identity_proof(None, &account, &pubkey)),
            CODE_IDENTITY_PROOF_INVALID
        );
        assert_eq!(
            code(require_identity_proof(Some("@@"), &account, &pubkey)),
            CODE_IDENTITY_PROOF_INVALID
        );
        assert_eq!(
            code(require_identity_proof(Some(&encoded), &[4; 16], &pubkey)),
            CODE_IDENTITY_PROOF_INVALID
        );
        let other = lakko_crypto::signing::SigningKey::from_seed(&[8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            code(require_identity_proof(Some(&encoded), &account, &other)),
            CODE_IDENTITY_PROOF_INVALID
        );
    }
}
