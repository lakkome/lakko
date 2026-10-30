// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The semantic half of the committer-delegation envelope ([ADR-0060] §6).
//!
//! `lakko_contacts::committer_delegation` carries a proposer's delegations for
//! whichever committer candidate sweeps its `Add`; that crate cannot depend
//! on this one, so its codec is structural only. This module does the rest,
//! once, for both FFI surfaces:
//!
//! * the added person's mailbox binding verifies under `added_account_key`
//!   ([ADR-0078]: the mailbox key is not derivable from the account key, so
//!   the envelope carries it, signed);
//! * every blob parses as a [`Delegation`];
//! * every delegation names the **added person's** mailbox
//!   (`mailbox_address(binding.mailbox_pk)`), so a proposer cannot hand a
//!   committer a delegation that points its write somewhere else;
//! * no two delegations name the same delegate, so "the one naming me" is
//!   unambiguous.
//!
//! An envelope with no delegation passes when its binding verifies: the
//! added person's mailbox is open (Everyone) and the committer writes the
//! Welcome there without a proof, spending a `mailbox-write` token.
//!
//! Any failure refuses the **whole** envelope: a partly bad envelope is from
//! a broken or hostile proposer, and the caller keeps the previous one.
//!
//! The signature and expiry are NOT checked here: the Delivery Service checks
//! both when the delegation is used, against the reach key registered for the
//! mailbox, and a committer cannot know which reach public key the added
//! person registered. [`delegation_for`] returns the delegation naming the
//! caller's own account key, which is the only one it can use.
//!
//! The caller still checks that `added_account_key` is the account the
//! proposal it rides beside adds (the roster); this module cannot.
//!
//! [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use lakko_contacts::committer_delegation::{
    CommitterDelegationEnvelope, ADDED_ACCOUNT_KEY_LEN, COMMITTER_DELEGATION_BLOB_LEN,
};

use crate::delegation::Delegation;
use crate::mailbox_address;

/// Why a committer-delegation envelope's delegations were refused. A reason
/// code and an index; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommitterDelegationCheckError {
    /// `added_account_key` is not a valid Ed25519 public key, so it names no
    /// mailbox.
    #[error("the added account key is not a valid public key")]
    BadAddedAccountKey,
    /// The added person's mailbox binding does not verify under
    /// `added_account_key` (ADR-0078).
    #[error("the added person's mailbox binding does not verify")]
    BadMailboxBinding,
    /// The blob at this index does not parse as a delegation.
    #[error("delegation {0} does not parse")]
    Malformed(usize),
    /// The delegation at this index names another mailbox than the added
    /// person's.
    #[error("delegation {0} names another mailbox")]
    WrongMailbox(usize),
    /// The delegation at this index names a delegate an earlier one already
    /// names.
    #[error("delegation {0} repeats a delegate")]
    DuplicateDelegate(usize),
}

impl CommitterDelegationCheckError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadAddedAccountKey => "badAddedAccountKey",
            Self::Malformed(_) => "malformed",
            Self::WrongMailbox(_) => "wrongMailbox",
            Self::DuplicateDelegate(_) => "duplicateDelegate",
            Self::BadMailboxBinding => "badMailboxBinding",
        }
    }
}

/// Parse and check every delegation of a structurally decoded envelope (the
/// module docs list the checks). Returns them in wire order.
///
/// # Errors
///
/// Every [`CommitterDelegationCheckError`]; the first failure refuses the
/// whole envelope.
pub fn check_committer_delegations(
    envelope: &CommitterDelegationEnvelope,
) -> Result<Vec<Delegation>, CommitterDelegationCheckError> {
    crate::VerifyingKey::from_bytes(&envelope.added_account_key)
        .map_err(|_| CommitterDelegationCheckError::BadAddedAccountKey)?;
    let mailbox_pk = envelope
        .added_mailbox_binding
        .verify(&envelope.added_account_key)
        .map_err(|_| CommitterDelegationCheckError::BadMailboxBinding)?;
    let mailbox = mailbox_address(&mailbox_pk);
    let mut out: Vec<Delegation> = Vec::with_capacity(envelope.delegations.len());
    for (index, blob) in envelope.delegations.iter().enumerate() {
        let delegation = Delegation::decode(blob)
            .map_err(|_| CommitterDelegationCheckError::Malformed(index))?;
        if delegation.mailbox != mailbox {
            return Err(CommitterDelegationCheckError::WrongMailbox(index));
        }
        if out.iter().any(|d| d.delegate_pk == delegation.delegate_pk) {
            return Err(CommitterDelegationCheckError::DuplicateDelegate(index));
        }
        out.push(delegation);
    }
    Ok(out)
}

/// The delegation naming `own_account_key` as its delegate, if any: the one
/// a committer candidate uses to deliver the Welcome of a sweep that added
/// the envelope's person.
#[must_use]
pub fn delegation_for<'a>(
    delegations: &'a [Delegation],
    own_account_key: &[u8; ADDED_ACCOUNT_KEY_LEN],
) -> Option<&'a Delegation> {
    delegations
        .iter()
        .find(|d| d.delegate_pk == *own_account_key)
}

// The envelope's blob length is this crate's wire length; a drift would make
// every envelope unparseable here while the codec there accepts it.
const _: () = assert!(COMMITTER_DELEGATION_BLOB_LEN == crate::delegation::DELEGATION_LEN);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::sign_delegation;
    use crate::SigningKey;

    use lakko_contacts::mailbox::{MailboxBinding, MailboxKey};

    fn account(byte: u8) -> [u8; 32] {
        SigningKey::from_seed(&[byte; 32])
            .verifying_key()
            .to_bytes()
    }

    /// The mailbox address of the account whose seed is `[byte; 32]`.
    fn mailbox_of(byte: u8) -> [u8; 32] {
        MailboxKey::from_account_seed(&[byte; 32]).address()
    }

    fn envelope(added: u8, delegates: &[[u8; 32]]) -> CommitterDelegationEnvelope {
        let mailbox = mailbox_of(added);
        CommitterDelegationEnvelope {
            added_account_key: account(added),
            added_mailbox_binding: MailboxBinding::for_account_seed(&[added; 32]),
            delegations: delegates
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    sign_delegation(
                        &[0x44; 32],
                        &mailbox,
                        d,
                        1_000,
                        [u8::try_from(i).unwrap(); 16],
                    )
                    .encode()
                })
                .collect(),
        }
    }

    #[test]
    fn the_blob_length_is_the_delegation_length() {
        assert_eq!(COMMITTER_DELEGATION_BLOB_LEN, crate::DELEGATION_LEN);
    }

    #[test]
    fn a_well_formed_envelope_checks_and_each_candidate_finds_its_own() {
        let owner = account(1);
        let admin = account(2);
        let env = envelope(9, &[owner, admin]);
        let parsed = check_committer_delegations(&env).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(delegation_for(&parsed, &owner).unwrap().delegate_pk, owner);
        assert_eq!(delegation_for(&parsed, &admin).unwrap().delegate_pk, admin);
        assert!(delegation_for(&parsed, &account(3)).is_none());
    }

    #[test]
    fn a_delegation_for_another_mailbox_refuses_the_envelope() {
        let mut env = envelope(9, &[account(1), account(2)]);
        env.delegations[1] =
            sign_delegation(&[0x44; 32], &mailbox_of(8), &account(2), 1_000, [7; 16]).encode();
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::WrongMailbox(1))
        );
    }

    #[test]
    fn a_binding_not_signed_by_the_added_account_refuses_the_envelope() {
        let mut env = envelope(9, &[account(1)]);
        env.added_mailbox_binding = MailboxBinding::for_account_seed(&[8; 32]);
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::BadMailboxBinding)
        );
        // The delegation names the added account's real mailbox, but the
        // envelope claims another account added it: the binding fails.
        let mut env = envelope(9, &[account(1)]);
        env.added_account_key = account(8);
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::BadMailboxBinding)
        );
    }

    #[test]
    fn an_open_mailbox_envelope_carries_no_delegation_but_a_checked_binding() {
        // A person in Everyone: the proposer holds no grant, so the envelope
        // names the mailbox through the binding alone and the committer
        // writes without a proof (lakko_contacts::committer_delegation).
        let env = envelope(9, &[]);
        let parsed = check_committer_delegations(&env).unwrap();
        assert!(parsed.is_empty());
        assert!(delegation_for(&parsed, &account(1)).is_none());
        // The binding is still checked: it can only name the added person's
        // own mailbox.
        let mut env = envelope(9, &[]);
        env.added_mailbox_binding = MailboxBinding::for_account_seed(&[8; 32]);
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::BadMailboxBinding)
        );
    }

    #[test]
    fn a_repeated_delegate_refuses_the_envelope() {
        let env = envelope(9, &[account(1), account(1)]);
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::DuplicateDelegate(1))
        );
    }

    #[test]
    fn an_unparseable_blob_refuses_the_envelope() {
        let mut env = envelope(9, &[account(1)]);
        env.delegations[0][0] = 0x7F;
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::Malformed(0))
        );
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(
            CommitterDelegationCheckError::BadAddedAccountKey.code(),
            "badAddedAccountKey"
        );
        assert_eq!(
            CommitterDelegationCheckError::Malformed(0).code(),
            "malformed"
        );
        assert_eq!(
            CommitterDelegationCheckError::WrongMailbox(0).code(),
            "wrongMailbox"
        );
        assert_eq!(
            CommitterDelegationCheckError::DuplicateDelegate(0).code(),
            "duplicateDelegate"
        );
        assert_eq!(
            CommitterDelegationCheckError::BadMailboxBinding.code(),
            "badMailboxBinding"
        );
    }
}
