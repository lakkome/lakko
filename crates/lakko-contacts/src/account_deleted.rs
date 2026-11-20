// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A deleted account
//! ([ADR-0098](../../../docs/adr/0098-deleted-account.md),
//! `docs/ux/deleted-account.md`).
//!
//! Two things live here, both written once so the clients cannot disagree:
//!
//! 1. **The account-deleted envelope** (push). After `DELETE /account`
//!    succeeded at auth and before the wipe, the deleting device posts it as
//!    an MLS application message in every conversation it holds. A receiver
//!    keeps it only when the MLS sender, resolved at receipt to an account key
//!    (C-68), is the key the signature verifies under, over this
//!    conversation's group id ([`verify_account_deleted`]). An accepted
//!    statement is final.
//! 2. **The liveness rules** (pull and display). A receiver that never got the
//!    envelope asks the directory for the person's identity bundle;
//!    [`account_deletion_status`] combines a statement with the latest answer,
//!    [`may_show_username`] says whether a `@username` may be shown beside the
//!    person, and [`directory_check_due`] says when to ask again.
//!
//! ## Wire format (v1)
//!
//! ```text
//! ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1 || version(1) || sig(64)
//! ```
//!
//! Fixed length: [`ACCOUNT_DELETED_ENVELOPE_LEN`] bytes. `sig` is Ed25519 by
//! the account key the sender's conversation leaves are bound under (the
//! ADR-0050 binding key) over
//!
//! ```text
//! ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1 || version || group_id_len(1) || group_id || account_key(32)
//! ```
//!
//! so a statement is bound to one conversation and one key. It carries no
//! time: the statement is final, nothing needs ordering.
//!
//! ## Why the username rule is a time bound
//!
//! The directory holds a deleted or renamed username for a hold `H` (30 days
//! by default, ADR-0076, ADR-0098 section 4) before anyone else may take it.
//! If the account was confirmed to exist at `L`, it was deleted or renamed at
//! `D >= L`, so its old name cannot be anyone else's before `L + H`. A name
//! shown only while `now < L + V` with `V` < `H` therefore never names someone
//! else. [`USERNAME_CONFIRMATION_VALID_MS`] is `V`.

use core::result::Result as CoreResult;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// 25-byte domain separator. The trailing `\x00` keeps a UTF-8 payload from
/// ever starting with it. Frozen; the version byte is the knob that moves.
pub const ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/account-deleted/v1\x00";

/// Schema-version byte carried first after the separator.
pub const ACCOUNT_DELETED_VERSION_V1: u8 = 0x01;

/// Bytes after the separator: the version and the signature.
const PAYLOAD_LEN: usize = 1 + SIGNATURE_LEN;

/// The one well-formed length of an envelope.
pub const ACCOUNT_DELETED_ENVELOPE_LEN: usize =
    ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.len() + PAYLOAD_LEN;

/// The longest group id a statement can be bound to (its length is one byte
/// of the signed message).
pub const ACCOUNT_DELETED_MAX_GROUP_ID_LEN: usize = 255;

/// Length of an account key (Ed25519 public key).
pub const ACCOUNT_KEY_LEN: usize = 32;

/// How long a directory confirmation that the person's account exists lets
/// this device show their `@username`: 21 days. Must stay below the
/// directory's username hold (30 days by default); the week between them is
/// the margin for clock skew. Chosen, not measured.
pub const USERNAME_CONFIRMATION_VALID_MS: u64 = 21 * 24 * 60 * 60 * 1000;

/// A directory answer older than this makes a new check due: 7 days. Chosen,
/// not measured.
pub const DIRECTORY_RECHECK_AFTER_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// No new check within this long of the last attempt, answered or not: one
/// hour. Chosen, not measured.
pub const DIRECTORY_RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

/// A decoded statement. Decoding checks the layout only; the signature is
/// checked by [`verify_account_deleted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeleted {
    /// Ed25519 signature by the deleted account's key.
    pub signature: [u8; SIGNATURE_LEN],
}

/// The bytes a statement signs.
fn signed_message(
    group_id: &[u8],
    account_key: &[u8; ACCOUNT_KEY_LEN],
) -> CoreResult<Vec<u8>, AccountDeletedError> {
    let len = u8::try_from(group_id.len())
        .ok()
        .filter(|&n| n > 0)
        .ok_or(AccountDeletedError::BadGroupId)?;
    let mut out = Vec::with_capacity(
        ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.len() + 2 + group_id.len() + ACCOUNT_KEY_LEN,
    );
    out.extend_from_slice(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1);
    out.push(ACCOUNT_DELETED_VERSION_V1);
    out.push(len);
    out.extend_from_slice(group_id);
    out.extend_from_slice(account_key);
    Ok(out)
}

/// Compose the statement for one conversation, signed by `account` (the key
/// this device's leaves in that conversation are bound under).
///
/// # Errors
///
/// [`AccountDeletedError::BadGroupId`] when `group_id` is empty or longer
/// than [`ACCOUNT_DELETED_MAX_GROUP_ID_LEN`].
pub fn compose_account_deleted(
    account: &SigningKey,
    group_id: &[u8],
) -> CoreResult<Vec<u8>, AccountDeletedError> {
    let key = account.verifying_key().to_bytes();
    let message = signed_message(group_id, &key)?;
    let sig = account.sign(&message);
    let mut out = Vec::with_capacity(ACCOUNT_DELETED_ENVELOPE_LEN);
    out.extend_from_slice(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1);
    out.push(ACCOUNT_DELETED_VERSION_V1);
    out.extend_from_slice(&sig.to_bytes());
    Ok(out)
}

/// Decode a statement's layout (no signature check). Fail-closed: the
/// domain, then the version (BEFORE the length, so a later revision that
/// changes the length reports as a version), then the exact length.
///
/// # Errors
///
/// [`AccountDeletedError::WrongDomain`],
/// [`AccountDeletedError::UnknownVersion`], [`AccountDeletedError::BadLength`].
pub fn decode_account_deleted(bytes: &[u8]) -> CoreResult<AccountDeleted, AccountDeletedError> {
    let payload = bytes
        .strip_prefix(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1)
        .ok_or(AccountDeletedError::WrongDomain)?;
    let bad = || AccountDeletedError::BadLength {
        actual: payload.len(),
    };
    let (&version, sig) = payload.split_first().ok_or_else(bad)?;
    if version != ACCOUNT_DELETED_VERSION_V1 {
        return Err(AccountDeletedError::UnknownVersion(version));
    }
    Ok(AccountDeleted {
        signature: sig.try_into().map_err(|_| bad())?,
    })
}

/// The dispatcher's arm: `Ok(None)` when the plaintext does not carry the
/// domain separator, `Ok(Some(_))` for a well-formed statement, `Err` when the
/// separator matched and the rest is malformed.
///
/// # Errors
///
/// Every [`decode_account_deleted`] error except
/// [`AccountDeletedError::WrongDomain`].
pub fn try_unwrap_account_deleted(
    plaintext: &[u8],
) -> CoreResult<Option<AccountDeleted>, AccountDeletedError> {
    if !plaintext.starts_with(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    decode_account_deleted(plaintext).map(Some)
}

/// Verify a statement received in the conversation `group_id` from the MLS
/// sender whose account key, resolved at receipt (C-68), is
/// `sender_account_key`. Every failure is a refusal; the caller changes
/// nothing.
///
/// # Errors
///
/// Every [`decode_account_deleted`] error,
/// [`AccountDeletedError::BadGroupId`], [`AccountDeletedError::BadKey`],
/// [`AccountDeletedError::BadSignature`].
pub fn verify_account_deleted(
    bytes: &[u8],
    group_id: &[u8],
    sender_account_key: &[u8],
) -> CoreResult<(), AccountDeletedError> {
    let statement = decode_account_deleted(bytes)?;
    let key_bytes: [u8; ACCOUNT_KEY_LEN] = sender_account_key
        .try_into()
        .map_err(|_| AccountDeletedError::BadKey)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| AccountDeletedError::BadKey)?;
    let message = signed_message(group_id, &key_bytes)?;
    let sig = Signature::from_bytes(&statement.signature)
        .map_err(|_| AccountDeletedError::BadSignature)?;
    key.verify(&message, &sig)
        .map_err(|_| AccountDeletedError::BadSignature)
}

/// Why a statement was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AccountDeletedError {
    /// The bytes do not carry the account-deleted domain separator.
    #[error("bytes do not carry the account-deleted domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("account-deleted statement has {actual} payload bytes, expected {PAYLOAD_LEN}")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The version byte is not [`ACCOUNT_DELETED_VERSION_V1`].
    #[error("account-deleted statement carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The group id is empty or longer than
    /// [`ACCOUNT_DELETED_MAX_GROUP_ID_LEN`].
    #[error("group id must be 1..=255 bytes")]
    BadGroupId,
    /// The sender key is not a valid 32-byte Ed25519 key.
    #[error("sender account key is not a valid Ed25519 key")]
    BadKey,
    /// The signature does not verify under the sender's account key over
    /// this conversation.
    #[error("account-deleted signature does not verify")]
    BadSignature,
}

impl AccountDeletedError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadGroupId => "badGroupId",
            Self::BadKey => "badKey",
            Self::BadSignature => "badSignature",
        }
    }
}

// =============================================================================
// Liveness rules
// =============================================================================

/// A complete answer of `GET /discovery/identity/{account_id}` for a person,
/// as the client classified it. Errors, timeouts and `5xx` are not answers
/// and are never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryAnswer {
    /// `404`: no identity bundle for that account id.
    Absent,
    /// `200`, and the bundle's key is the key this device holds for the
    /// person, or a conversation link in the bundle (ADR-0074 section 3.7)
    /// verifies naming that key.
    SamePerson,
    /// `200` with another key and no link to the held key: a reset or a
    /// rotation, judged by the ADR-0016 / ADR-0073 gates. Never a deletion.
    OtherKey,
}

/// Whether a person's account exists, as this device knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// Shown as themselves.
    Active,
    /// Shown as "Deleted account".
    Deleted,
}

/// The person's status: a verified statement (section 1 of ADR-0098) is
/// final; otherwise the latest recorded directory answer decides, `Absent`
/// meaning deleted. No answer is `Active`.
#[must_use]
pub const fn account_deletion_status(
    statement_verified: bool,
    last_answer: Option<DirectoryAnswer>,
) -> AccountStatus {
    if statement_verified {
        return AccountStatus::Deleted;
    }
    match last_answer {
        Some(DirectoryAnswer::Absent) => AccountStatus::Deleted,
        Some(DirectoryAnswer::SamePerson | DirectoryAnswer::OtherKey) | None => {
            AccountStatus::Active
        },
    }
}

/// Whether a `@username` may be shown beside the person now. Never for a
/// deleted person; otherwise only while the latest `SamePerson` answer
/// (`confirmed_at_ms`, this device's clock) is younger than
/// [`USERNAME_CONFIRMATION_VALID_MS`]. A confirmation in the future (the clock
/// went back) counts as none. The caller still requires the username
/// certificate to verify; this rule is in addition.
#[must_use]
pub const fn may_show_username(
    status: AccountStatus,
    confirmed_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if matches!(status, AccountStatus::Deleted) {
        return false;
    }
    match confirmed_at_ms {
        Some(at) if at <= now_ms => now_ms - at < USERNAME_CONFIRMATION_VALID_MS,
        _ => false,
    }
}

/// Whether to ask the directory about this person now. Never after a
/// verified statement; never within [`DIRECTORY_RETRY_AFTER_MS`] of the last
/// attempt (answered or not); otherwise due when there is no answer, or the
/// last answer is [`DIRECTORY_RECHECK_AFTER_MS`] old, or dated in the future.
/// An attempt dated in the future does not hold a check back.
#[must_use]
pub const fn directory_check_due(
    statement_verified: bool,
    last_answer_at_ms: Option<u64>,
    last_attempt_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if statement_verified {
        return false;
    }
    if let Some(at) = last_attempt_at_ms {
        if at <= now_ms && now_ms - at < DIRECTORY_RETRY_AFTER_MS {
            return false;
        }
    }
    match last_answer_at_ms {
        Some(at) if at <= now_ms => now_ms - at >= DIRECTORY_RECHECK_AFTER_MS,
        _ => true,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn account() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    fn key_of(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }

    const GROUP: [u8; 32] = [0x33; 32];

    #[test]
    fn a_composed_statement_has_the_one_length_and_verifies() {
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        assert_eq!(wire.len(), ACCOUNT_DELETED_ENVELOPE_LEN);
        assert_eq!(ACCOUNT_DELETED_ENVELOPE_LEN, 90);
        assert_eq!(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.len(), 25);
        assert!(wire.starts_with(b"lakko/"));
        verify_account_deleted(&wire, &GROUP, &key_of(&account())).unwrap();
    }

    #[test]
    fn a_statement_from_another_sender_is_refused() {
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        let other = SigningKey::from_seed(&[8u8; 32]);
        assert_eq!(
            verify_account_deleted(&wire, &GROUP, &key_of(&other)),
            Err(AccountDeletedError::BadSignature)
        );
    }

    #[test]
    fn a_statement_moved_into_another_conversation_is_refused() {
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        assert_eq!(
            verify_account_deleted(&wire, &[0x34; 32], &key_of(&account())),
            Err(AccountDeletedError::BadSignature)
        );
    }

    #[test]
    fn group_ids_out_of_bounds_are_refused_both_ways() {
        assert_eq!(
            compose_account_deleted(&account(), &[]),
            Err(AccountDeletedError::BadGroupId)
        );
        assert_eq!(
            compose_account_deleted(&account(), &[1u8; 256]),
            Err(AccountDeletedError::BadGroupId)
        );
        let max = compose_account_deleted(&account(), &[1u8; 255]).unwrap();
        verify_account_deleted(&max, &[1u8; 255], &key_of(&account())).unwrap();
        assert_eq!(
            verify_account_deleted(&max, &[], &key_of(&account())),
            Err(AccountDeletedError::BadGroupId)
        );
    }

    #[test]
    fn a_bad_key_is_refused() {
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        assert_eq!(
            verify_account_deleted(&wire, &GROUP, &[1u8; 31]),
            Err(AccountDeletedError::BadKey)
        );
    }

    #[test]
    fn the_layout_is_checked_domain_then_version_then_length() {
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        assert_eq!(
            decode_account_deleted(b"hello"),
            Err(AccountDeletedError::WrongDomain)
        );
        let mut v2 = wire.clone();
        v2[ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert_eq!(
            decode_account_deleted(&v2),
            Err(AccountDeletedError::UnknownVersion(0x02))
        );
        let mut short = wire.clone();
        short.pop();
        assert_eq!(
            decode_account_deleted(&short),
            Err(AccountDeletedError::BadLength { actual: 64 })
        );
        let mut long = wire;
        long.push(0);
        assert_eq!(
            decode_account_deleted(&long),
            Err(AccountDeletedError::BadLength { actual: 66 })
        );
        assert_eq!(
            decode_account_deleted(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1),
            Err(AccountDeletedError::BadLength { actual: 0 })
        );
    }

    #[test]
    fn try_unwrap_is_none_for_other_plaintexts() {
        assert_eq!(try_unwrap_account_deleted(b"just text"), Ok(None));
        assert_eq!(try_unwrap_account_deleted(b""), Ok(None));
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        assert!(try_unwrap_account_deleted(&wire).unwrap().is_some());
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(AccountDeletedError::BadSignature.code(), "badSignature");
        assert_eq!(AccountDeletedError::BadGroupId.code(), "badGroupId");
    }

    #[test]
    fn status_statement_is_final_and_absent_is_deleted() {
        use AccountStatus::{Active, Deleted};
        use DirectoryAnswer::{Absent, OtherKey, SamePerson};
        assert_eq!(account_deletion_status(false, None), Active);
        assert_eq!(account_deletion_status(false, Some(Absent)), Deleted);
        assert_eq!(account_deletion_status(false, Some(SamePerson)), Active);
        assert_eq!(account_deletion_status(false, Some(OtherKey)), Active);
        for answer in [None, Some(Absent), Some(SamePerson), Some(OtherKey)] {
            assert_eq!(account_deletion_status(true, answer), Deleted);
        }
    }

    #[test]
    fn username_needs_a_recent_confirmation_and_an_active_account() {
        let now = 100 * 24 * 60 * 60 * 1000;
        let active = AccountStatus::Active;
        assert!(!may_show_username(active, None, now));
        assert!(may_show_username(active, Some(now), now));
        assert!(may_show_username(
            active,
            Some(now - USERNAME_CONFIRMATION_VALID_MS + 1),
            now
        ));
        assert!(!may_show_username(
            active,
            Some(now - USERNAME_CONFIRMATION_VALID_MS),
            now
        ));
        assert!(!may_show_username(active, Some(now + 1), now));
        assert!(!may_show_username(AccountStatus::Deleted, Some(now), now));
    }

    #[test]
    fn the_username_window_is_below_the_directory_hold() {
        // ADR-0076 default hold: 30 days (services/discovery registry).
        const HOLD_MS: u64 = 30 * 24 * 60 * 60 * 1000;
        const { assert!(USERNAME_CONFIRMATION_VALID_MS < HOLD_MS) };
        const { assert!(DIRECTORY_RECHECK_AFTER_MS < USERNAME_CONFIRMATION_VALID_MS) };
    }

    #[test]
    fn check_due_rules() {
        let now = 50 * 24 * 60 * 60 * 1000;
        assert!(directory_check_due(false, None, None, now));
        assert!(!directory_check_due(true, None, None, now));
        // A fresh answer: not due; an old one: due.
        assert!(!directory_check_due(false, Some(now - 1000), None, now));
        assert!(directory_check_due(
            false,
            Some(now - DIRECTORY_RECHECK_AFTER_MS),
            None,
            now
        ));
        // A recent attempt holds even a due check back.
        assert!(!directory_check_due(false, None, Some(now - 1000), now));
        assert!(directory_check_due(
            false,
            None,
            Some(now - DIRECTORY_RETRY_AFTER_MS),
            now
        ));
        // Clock went back: the future answer makes the check due, the future
        // attempt does not hold it back.
        assert!(directory_check_due(
            false,
            Some(now + 5),
            Some(now + 5),
            now
        ));
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::group_reactions::GROUP_REACTIONS_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::link_founded::LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            crate::link_refusal::LINK_REFUSAL_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
        ];
        for d in other_domains {
            assert!(
                !d.starts_with(ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1)
                    && !ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn the_statement_dispatches_as_its_own_kind() {
        use crate::incoming_envelope::{
            dispatch_incoming_envelope, DispatchError, IncomingEnvelope,
        };
        let wire = compose_account_deleted(&account(), &GROUP).unwrap();
        let expected = decode_account_deleted(&wire).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire).unwrap(),
            IncomingEnvelope::AccountDeleted(expected)
        );
        let mut broken = wire;
        broken.pop();
        assert!(matches!(
            dispatch_incoming_envelope(&broken),
            Err(DispatchError::AccountDeleted(
                AccountDeletedError::BadLength { .. }
            ))
        ));
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_accepts_only_the_one_length(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            if decode_account_deleted(&bytes).is_ok() {
                prop_assert_eq!(bytes.len(), ACCOUNT_DELETED_ENVELOPE_LEN);
            }
            let _ = verify_account_deleted(&bytes, &GROUP, &key_of(&account()));
        }

        #[test]
        fn a_statement_verifies_only_for_its_group_and_key(
            seed in any::<[u8; 32]>(),
            other_seed in any::<[u8; 32]>(),
            group in proptest::collection::vec(any::<u8>(), 1..=255),
            other_group in proptest::collection::vec(any::<u8>(), 1..=255),
        ) {
            let k = SigningKey::from_seed(&seed);
            let wire = compose_account_deleted(&k, &group).unwrap();
            prop_assert!(verify_account_deleted(&wire, &group, &key_of(&k)).is_ok());
            if other_group != group {
                prop_assert!(verify_account_deleted(&wire, &other_group, &key_of(&k)).is_err());
            }
            if other_seed != seed {
                let o = SigningKey::from_seed(&other_seed);
                prop_assert!(verify_account_deleted(&wire, &group, &key_of(&o)).is_err());
            }
        }

        #[test]
        fn any_flipped_byte_is_refused(byte in 0usize..ACCOUNT_DELETED_ENVELOPE_LEN, bit in 0u8..8) {
            let mut wire = compose_account_deleted(&account(), &GROUP).unwrap();
            wire[byte] ^= 1 << bit;
            prop_assert!(verify_account_deleted(&wire, &GROUP, &key_of(&account())).is_err());
        }

        #[test]
        fn a_shown_username_always_has_a_confirmation_inside_the_window(
            deleted in any::<bool>(),
            confirmed in proptest::option::of(any::<u64>()),
            now in any::<u64>(),
        ) {
            let status = if deleted { AccountStatus::Deleted } else { AccountStatus::Active };
            if may_show_username(status, confirmed, now) {
                prop_assert!(!deleted);
                let at = confirmed.unwrap();
                prop_assert!(at <= now && now - at < USERNAME_CONFIRMATION_VALID_MS);
            }
        }
    }
}
