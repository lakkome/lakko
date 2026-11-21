// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm surface for a deleted account (ADR-0098,
//! `docs/ux/deleted-account.md`): the account-deleted envelope and the shared
//! rules that decide whether a person is shown as "Deleted account" and
//! whether their `@username` may be shown. Mirror of
//! `crates/lakko-ffi-uniffi/src/account_deleted.rs`.

use lakko_contacts::account_deleted::{
    account_deletion_status as rust_status, compose_account_deleted as rust_compose,
    decode_account_deleted as rust_decode, deletion_effects as rust_deletion_effects,
    directory_check_due as rust_check_due, may_show_username as rust_may_show_username,
    verify_account_deleted as rust_verify, AccountStatus as RustAccountStatus,
    DirectoryAnswer as RustDirectoryAnswer, ACCOUNT_DELETED_ENVELOPE_LEN,
    DIRECTORY_RECHECK_AFTER_MS, DIRECTORY_RETRY_AFTER_MS, USERNAME_CONFIRMATION_VALID_MS,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

/// Compose the account-deleted envelope for the conversation `groupId`,
/// signed by `accountSigningSeed`: the 32-byte Ed25519 seed of the account
/// key this device's leaves in that conversation are bound under. Send it as
/// an ordinary application message, after `DELETE /account` succeeded and
/// before the wipe.
///
/// # Errors
///
/// `bounds: ...` for a seed that is not 32 bytes; `accountDeleted.badGroupId:
/// ...` for an empty or over-255-byte group id.
#[wasm_bindgen(js_name = composeAccountDeletedEnvelope)]
pub fn compose_account_deleted_envelope(
    account_signing_seed: &[u8],
    group_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    let seed = <[u8; 32]>::try_from(account_signing_seed)
        .map_err(|_| JsError::new("bounds: account seed must be 32 bytes"))?;
    rust_compose(&SigningKey::from_seed(&seed), group_id)
        .map_err(|e| JsError::new(&format!("accountDeleted.{}: {e}", e.code())))
}

/// `true` when `plaintext` is an account-deleted statement for the
/// conversation `groupId`, signed by `senderAccountKey` (the MLS sender's
/// account key resolved at receipt, C-68). Anything else is `false`. A `true`
/// is final: mark that person deleted for good.
#[wasm_bindgen(js_name = verifyAccountDeletedEnvelope)]
#[must_use]
pub fn verify_account_deleted_envelope(
    plaintext: &[u8],
    group_id: &[u8],
    sender_account_key: &[u8],
) -> bool {
    rust_verify(plaintext, group_id, sender_account_key).is_ok()
}

/// `true` when `plaintext` has the account-deleted layout (no signature
/// check). For filters that only need to stay silent about it.
#[wasm_bindgen(js_name = isAccountDeletedEnvelope)]
#[must_use]
pub fn is_account_deleted_envelope(plaintext: &[u8]) -> bool {
    rust_decode(plaintext).is_ok()
}

/// The one length of an account-deleted envelope.
#[wasm_bindgen(js_name = accountDeletedEnvelopeLen)]
#[must_use]
pub fn account_deleted_envelope_len() -> u32 {
    u32::try_from(ACCOUNT_DELETED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// A complete answer of `GET /discovery/identity/{account_id}`. Errors,
/// timeouts and `5xx` are not answers; record nothing.
#[wasm_bindgen(js_name = DirectoryAnswer)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryAnswer {
    /// `404`.
    Absent = 0,
    /// `200`, and the bundle's key is the key held for this person, or a
    /// conversation link in the bundle verifies naming it.
    SamePerson = 1,
    /// `200` with another key and no link to the held key (never a
    /// deletion).
    OtherKey = 2,
}

impl From<DirectoryAnswer> for RustDirectoryAnswer {
    fn from(a: DirectoryAnswer) -> Self {
        match a {
            DirectoryAnswer::Absent => Self::Absent,
            DirectoryAnswer::SamePerson => Self::SamePerson,
            DirectoryAnswer::OtherKey => Self::OtherKey,
        }
    }
}

/// Whether a person's account exists, as this device knows it.
#[wasm_bindgen(js_name = AccountStatus)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    /// Shown as themselves.
    Active = 0,
    /// Shown as "Deleted account".
    Deleted = 1,
}

impl From<RustAccountStatus> for AccountStatus {
    fn from(s: RustAccountStatus) -> Self {
        match s {
            RustAccountStatus::Active => Self::Active,
            RustAccountStatus::Deleted => Self::Deleted,
        }
    }
}

impl From<AccountStatus> for RustAccountStatus {
    fn from(s: AccountStatus) -> Self {
        match s {
            AccountStatus::Active => Self::Active,
            AccountStatus::Deleted => Self::Deleted,
        }
    }
}

/// What a client does about a person who may have deleted their account
/// (ADR-0098, security review 2026-11-21).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct DeletionEffects {
    show_deleted: bool,
    remove_from_groups: bool,
    drop_held_sends: bool,
}

#[wasm_bindgen]
impl DeletionEffects {
    /// Show "Deleted account" in place of the person's name.
    #[wasm_bindgen(getter, js_name = showDeleted)]
    #[must_use]
    pub fn show_deleted(&self) -> bool {
        self.show_deleted
    }

    /// An owner's or admin's device proposes the removal of the person's
    /// leaves from the groups it administers.
    #[wasm_bindgen(getter, js_name = removeFromGroups)]
    #[must_use]
    pub fn remove_from_groups(&self) -> bool {
        self.remove_from_groups
    }

    /// Messages held for the person that were never sent may be dropped.
    #[wasm_bindgen(getter, js_name = dropHeldSends)]
    #[must_use]
    pub fn drop_held_sends(&self) -> bool {
        self.drop_held_sends
    }
}

/// The one rule for what a deletion does on this device: show "Deleted
/// account" on a verified statement or an `Absent` answer, unless the latest
/// answer is `SamePerson`; remove the person from groups and drop held sends
/// only on a verified statement AND an `Absent` answer. Clear the recorded
/// answer when recording a statement, so a `SamePerson` that counts is one
/// received after it. `undefined` is no answer.
#[wasm_bindgen(js_name = deletionEffects)]
#[must_use]
pub fn deletion_effects(
    statement_verified: bool,
    last_directory_answer: Option<DirectoryAnswer>,
) -> DeletionEffects {
    let e = rust_deletion_effects(statement_verified, last_directory_answer.map(Into::into));
    DeletionEffects {
        show_deleted: e.show_deleted,
        remove_from_groups: e.remove_from_groups,
        drop_held_sends: e.drop_held_sends,
    }
}

/// The person's displayed status: `deletionEffects(...).showDeleted`;
/// `undefined` (no answer) and no statement is `Active`.
#[wasm_bindgen(js_name = accountDeletionStatus)]
#[must_use]
pub fn account_deletion_status(
    statement_verified: bool,
    last_directory_answer: Option<DirectoryAnswer>,
) -> AccountStatus {
    rust_status(statement_verified, last_directory_answer.map(Into::into)).into()
}

/// Whether a `@username` may be shown beside the person now: never when
/// deleted; otherwise only while the latest `SamePerson` answer
/// (`confirmedAtMs`, this device's clock, `undefined` when none) is younger
/// than `usernameConfirmationValidMs()`.
#[wasm_bindgen(js_name = mayShowUsername)]
#[must_use]
pub fn may_show_username(status: AccountStatus, confirmed_at_ms: Option<u64>, now_ms: u64) -> bool {
    rust_may_show_username(status.into(), confirmed_at_ms, now_ms)
}

/// Whether to ask the directory about this person now (only for a person on
/// screen). Never within `directoryRetryAfterMs()` of the last attempt;
/// otherwise due when there is no answer or the last one is
/// `directoryRecheckAfterMs()` old. A verified statement no longer stops the
/// checks (its destructive effects need an `Absent` answer too);
/// `statementVerified` decides nothing.
#[wasm_bindgen(js_name = directoryCheckDue)]
#[must_use]
pub fn directory_check_due(
    statement_verified: bool,
    last_answer_at_ms: Option<u64>,
    last_attempt_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    rust_check_due(
        statement_verified,
        last_answer_at_ms,
        last_attempt_at_ms,
        now_ms,
    )
}

/// How long a `SamePerson` answer lets a username be shown (21 days).
#[wasm_bindgen(js_name = usernameConfirmationValidMs)]
#[must_use]
pub fn username_confirmation_valid_ms() -> u64 {
    USERNAME_CONFIRMATION_VALID_MS
}

/// An answer older than this makes a new check due (7 days).
#[wasm_bindgen(js_name = directoryRecheckAfterMs)]
#[must_use]
pub fn directory_recheck_after_ms() -> u64 {
    DIRECTORY_RECHECK_AFTER_MS
}

/// No new check within this long of the last attempt (1 hour).
#[wasm_bindgen(js_name = directoryRetryAfterMs)]
#[must_use]
pub fn directory_retry_after_ms() -> u64 {
    DIRECTORY_RETRY_AFTER_MS
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn key(seed: u8) -> Vec<u8> {
        SigningKey::from_seed(&[seed; 32])
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn a_statement_composed_here_verifies_only_for_its_sender_and_group() {
        let wire = compose_account_deleted_envelope(&[7; 32], &[0x33; 32]).unwrap();
        assert_eq!(wire.len(), account_deleted_envelope_len() as usize);
        assert!(is_account_deleted_envelope(&wire));
        assert!(verify_account_deleted_envelope(&wire, &[0x33; 32], &key(7)));
        assert!(!verify_account_deleted_envelope(
            &wire,
            &[0x33; 32],
            &key(8)
        ));
        assert!(!verify_account_deleted_envelope(
            &wire,
            &[0x34; 32],
            &key(7)
        ));
    }

    #[test]
    fn the_rules_cross_the_boundary_unchanged() {
        assert_eq!(
            account_deletion_status(false, Some(DirectoryAnswer::Absent)),
            AccountStatus::Deleted
        );
        assert_eq!(account_deletion_status(false, None), AccountStatus::Active);
        let now = 1_000_000_000_000;
        assert!(may_show_username(AccountStatus::Active, Some(now), now));
        assert!(!may_show_username(AccountStatus::Active, None, now));
        assert!(directory_check_due(false, None, None, now));
        assert!(directory_check_due(true, None, None, now));
        let e = deletion_effects(true, Some(DirectoryAnswer::Absent));
        assert!(e.show_deleted() && e.remove_from_groups() && e.drop_held_sends());
        let e = deletion_effects(true, None);
        assert!(e.show_deleted() && !e.remove_from_groups() && !e.drop_held_sends());
        assert!(!deletion_effects(true, Some(DirectoryAnswer::SamePerson)).show_deleted());
    }
}
