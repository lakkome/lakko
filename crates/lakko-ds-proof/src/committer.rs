// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The semantic half of the committer-delegation envelope ([ADR-0058] §6).
//!
//! `lakko_contacts::committer_delegation` carries a proposer's delegations for
//! whichever committer candidate sweeps its `Add`; that crate cannot depend
//! on this one (this one names mailboxes with its pseudonym derivation), so
//! its codec is structural only. This module does the rest, once, for both
//! FFI surfaces:
//!
//! * every blob parses as a [`Delegation`];
//! * every delegation names the **added person's** mailbox
//!   (`mailbox_pseudonym(added_account_key)`), so a proposer cannot hand a
//!   committer a delegation that points its write somewhere else;
//! * no two delegations name the same delegate, so "the one naming me" is
//!   unambiguous.
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
//! [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md

use lakko_contacts::committer_delegation::{
    CommitterDelegationEnvelope, ADDED_ACCOUNT_KEY_LEN, COMMITTER_DELEGATION_BLOB_LEN,
};

use crate::delegation::Delegation;
use crate::{mailbox_pseudonym, VerifyingKey};

/// Why a committer-delegation envelope's delegations were refused. A reason
/// code and an index; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CommitterDelegationCheckError {
    /// `added_account_key` is not a valid Ed25519 public key, so it names no
    /// mailbox.
    #[error("the added account key is not a valid public key")]
    BadAddedAccountKey,
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
    let added = VerifyingKey::from_bytes(&envelope.added_account_key)
        .map_err(|_| CommitterDelegationCheckError::BadAddedAccountKey)?;
    let mailbox = mailbox_pseudonym(&added);
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

    fn account(byte: u8) -> [u8; 32] {
        SigningKey::from_seed(&[byte; 32])
            .verifying_key()
            .to_bytes()
    }

    fn mailbox_of(key: &[u8; 32]) -> [u8; 32] {
        mailbox_pseudonym(&VerifyingKey::from_bytes(key).unwrap())
    }

    fn envelope(added: [u8; 32], delegates: &[[u8; 32]]) -> CommitterDelegationEnvelope {
        let mailbox = mailbox_of(&added);
        CommitterDelegationEnvelope {
            added_account_key: added,
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
        let env = envelope(account(9), &[owner, admin]);
        let parsed = check_committer_delegations(&env).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(delegation_for(&parsed, &owner).unwrap().delegate_pk, owner);
        assert_eq!(delegation_for(&parsed, &admin).unwrap().delegate_pk, admin);
        assert!(delegation_for(&parsed, &account(3)).is_none());
    }

    #[test]
    fn a_delegation_for_another_mailbox_refuses_the_envelope() {
        let mut env = envelope(account(9), &[account(1), account(2)]);
        env.delegations[1] = sign_delegation(
            &[0x44; 32],
            &mailbox_of(&account(8)),
            &account(2),
            1_000,
            [7; 16],
        )
        .encode();
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::WrongMailbox(1))
        );
    }

    #[test]
    fn a_repeated_delegate_refuses_the_envelope() {
        let env = envelope(account(9), &[account(1), account(1)]);
        assert_eq!(
            check_committer_delegations(&env),
            Err(CommitterDelegationCheckError::DuplicateDelegate(1))
        );
    }

    #[test]
    fn an_unparseable_blob_refuses_the_envelope() {
        let mut env = envelope(account(9), &[account(1)]);
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
    }
}
