// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for a deleted account (ADR-0098,
//! `docs/ux/deleted-account.md`): the account-deleted envelope a deleting
//! device posts in every conversation, and the shared rules that decide
//! whether a person is shown as "Deleted account" and whether their
//! `@username` may be shown. Mirror of `crates/lakko-ffi-wasm/src/account_deleted.rs`.

use lakko_contacts::account_deleted::{
    account_deletion_status as rust_status, compose_account_deleted as rust_compose,
    decode_account_deleted as rust_decode, directory_check_due as rust_check_due,
    may_show_username as rust_may_show_username, verify_account_deleted as rust_verify,
    AccountDeletedError, AccountStatus as RustAccountStatus,
    DirectoryAnswer as RustDirectoryAnswer, ACCOUNT_DELETED_ENVELOPE_LEN,
    DIRECTORY_RECHECK_AFTER_MS, DIRECTORY_RETRY_AFTER_MS, USERNAME_CONFIRMATION_VALID_MS,
};
use lakko_crypto::signing::SigningKey;

use crate::FfiError;

fn error(e: AccountDeletedError) -> FfiError {
    FfiError::Contacts(format!("accountDeleted.{}: {e}", e.code()))
}

/// Compose the account-deleted envelope for the conversation `group_id`,
/// signed by `account_signing_seed`: the 32-byte Ed25519 seed of the account
/// key this device's leaves in that conversation are bound under (the
/// ADR-0050 binding key; after an ADR-0074 rotation, the conversation key).
/// Send it as an ordinary application message, after `DELETE /account`
/// succeeded and before the wipe.
///
/// # Errors
///
/// [`FfiError::Contacts`] (`badLength: ...` for a seed that is not 32 bytes,
/// `accountDeleted.badGroupId` for an empty or over-255-byte group id).
#[uniffi::export]
pub fn compose_account_deleted_envelope(
    account_signing_seed: Vec<u8>,
    group_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed = <[u8; 32]>::try_from(account_signing_seed.as_slice()).map_err(|_| {
        FfiError::Contacts(format!(
            "badLength: account seed must be 32 bytes, got {}",
            account_signing_seed.len()
        ))
    })?;
    rust_compose(&SigningKey::from_seed(&seed), &group_id).map_err(error)
}

/// `true` when `plaintext` is an account-deleted statement for the
/// conversation `group_id`, signed by `sender_account_key`: the MLS sender's
/// account key resolved at receipt (C-68). Anything else, inputs of the wrong
/// length included, is `false`: the caller changes nothing. A `true` is
/// final: mark that person deleted for good.
#[uniffi::export]
#[must_use]
pub fn verify_account_deleted_envelope(
    plaintext: Vec<u8>,
    group_id: Vec<u8>,
    sender_account_key: Vec<u8>,
) -> bool {
    rust_verify(&plaintext, &group_id, &sender_account_key).is_ok()
}

/// `true` when `plaintext` has the account-deleted layout (no signature
/// check). For filters that only need to stay silent about it.
#[uniffi::export]
#[must_use]
pub fn is_account_deleted_envelope(plaintext: Vec<u8>) -> bool {
    rust_decode(&plaintext).is_ok()
}

/// The one length of an account-deleted envelope.
#[uniffi::export]
#[must_use]
pub fn account_deleted_envelope_len() -> u32 {
    u32::try_from(ACCOUNT_DELETED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// A complete answer of `GET /discovery/identity/{account_id}` as the client
/// classified it. Errors, timeouts and `5xx` are not answers; record nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDirectoryAnswer {
    /// `404`.
    Absent,
    /// `200`, and the bundle's key is the key held for this person, or a
    /// conversation link in the bundle verifies naming it.
    SamePerson,
    /// `200` with another key and no link to the held key (a reset or a
    /// rotation; never a deletion).
    OtherKey,
}

impl From<FfiDirectoryAnswer> for RustDirectoryAnswer {
    fn from(a: FfiDirectoryAnswer) -> Self {
        match a {
            FfiDirectoryAnswer::Absent => Self::Absent,
            FfiDirectoryAnswer::SamePerson => Self::SamePerson,
            FfiDirectoryAnswer::OtherKey => Self::OtherKey,
        }
    }
}

/// Whether a person's account exists, as this device knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAccountStatus {
    /// Shown as themselves.
    Active,
    /// Shown as "Deleted account".
    Deleted,
}

impl From<RustAccountStatus> for FfiAccountStatus {
    fn from(s: RustAccountStatus) -> Self {
        match s {
            RustAccountStatus::Active => Self::Active,
            RustAccountStatus::Deleted => Self::Deleted,
        }
    }
}

impl From<FfiAccountStatus> for RustAccountStatus {
    fn from(s: FfiAccountStatus) -> Self {
        match s {
            FfiAccountStatus::Active => Self::Active,
            FfiAccountStatus::Deleted => Self::Deleted,
        }
    }
}

/// The person's status: a verified statement is final; otherwise the latest
/// recorded directory answer decides (`absent` = deleted). No answer is
/// `active`.
#[uniffi::export]
#[must_use]
pub fn account_deletion_status(
    statement_verified: bool,
    last_directory_answer: Option<FfiDirectoryAnswer>,
) -> FfiAccountStatus {
    rust_status(statement_verified, last_directory_answer.map(Into::into)).into()
}

/// Whether a `@username` may be shown beside the person now: never when
/// deleted; otherwise only while the latest `samePerson` answer
/// (`confirmed_at_ms`, this device's clock) is younger than
/// `usernameConfirmationValidMs`. Required in addition to the username
/// certificate verifying.
#[uniffi::export]
#[must_use]
pub fn may_show_username(
    status: FfiAccountStatus,
    confirmed_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    rust_may_show_username(status.into(), confirmed_at_ms, now_ms)
}

/// Whether to ask the directory about this person now (only for a person on
/// screen). Never after a verified statement; never within
/// `directoryRetryAfterMs` of the last attempt; otherwise due when there is no
/// answer or the last one is `directoryRecheckAfterMs` old.
#[uniffi::export]
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

/// How long a `samePerson` answer lets a username be shown (21 days).
#[uniffi::export]
#[must_use]
pub const fn username_confirmation_valid_ms() -> u64 {
    USERNAME_CONFIRMATION_VALID_MS
}

/// An answer older than this makes a new check due (7 days).
#[uniffi::export]
#[must_use]
pub const fn directory_recheck_after_ms() -> u64 {
    DIRECTORY_RECHECK_AFTER_MS
}

/// No new check within this long of the last attempt (1 hour).
#[uniffi::export]
#[must_use]
pub const fn directory_retry_after_ms() -> u64 {
    DIRECTORY_RETRY_AFTER_MS
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::contacts::{dispatch_incoming_envelope, FfiIncomingEnvelopeKind};

    fn key(seed: u8) -> Vec<u8> {
        SigningKey::from_seed(&[seed; 32])
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn a_statement_composed_here_verifies_only_for_its_sender_and_group() {
        let group = vec![0x33; 32];
        let wire = compose_account_deleted_envelope(vec![7; 32], group.clone()).unwrap();
        assert_eq!(wire.len(), account_deleted_envelope_len() as usize);
        assert!(is_account_deleted_envelope(wire.clone()));
        assert!(verify_account_deleted_envelope(
            wire.clone(),
            group.clone(),
            key(7)
        ));
        assert!(!verify_account_deleted_envelope(
            wire.clone(),
            group,
            key(8)
        ));
        assert!(!verify_account_deleted_envelope(
            wire.clone(),
            vec![0x34; 32],
            key(7)
        ));
        assert!(!verify_account_deleted_envelope(
            wire,
            vec![0x33; 32],
            vec![1; 3]
        ));
        assert!(compose_account_deleted_envelope(vec![7; 31], vec![1]).is_err());
        assert!(compose_account_deleted_envelope(vec![7; 32], vec![]).is_err());
    }

    #[test]
    fn the_statement_dispatches_as_its_own_kind_and_is_never_a_chat_bubble() {
        let wire = compose_account_deleted_envelope(vec![7; 32], vec![1; 32]).unwrap();
        let env = dispatch_incoming_envelope(wire.clone()).unwrap();
        assert_eq!(env.kind, FfiIncomingEnvelopeKind::AccountDeleted);
        assert_eq!(env.payload, wire);
        let body = crate::attachments::parse_body_envelope(wire);
        assert_eq!(
            body.kind,
            crate::attachments::MessageBodyKind::ServiceEnvelope
        );
    }

    #[test]
    fn the_rules_cross_the_boundary_unchanged() {
        assert_eq!(
            account_deletion_status(false, Some(FfiDirectoryAnswer::Absent)),
            FfiAccountStatus::Deleted
        );
        assert_eq!(
            account_deletion_status(false, Some(FfiDirectoryAnswer::OtherKey)),
            FfiAccountStatus::Active
        );
        assert_eq!(
            account_deletion_status(true, None),
            FfiAccountStatus::Deleted
        );
        let now = 1_000_000_000_000;
        assert!(may_show_username(FfiAccountStatus::Active, Some(now), now));
        assert!(!may_show_username(
            FfiAccountStatus::Deleted,
            Some(now),
            now
        ));
        assert!(!may_show_username(
            FfiAccountStatus::Active,
            Some(now - username_confirmation_valid_ms()),
            now
        ));
        assert!(directory_check_due(false, None, None, now));
        assert!(!directory_check_due(true, None, None, now));
        assert!(!directory_check_due(false, Some(now), None, now));
        assert!(directory_retry_after_ms() < directory_recheck_after_ms());
    }
}
