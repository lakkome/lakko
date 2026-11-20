// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A moderator's action ([ADR-0089] section 4).
//!
//! ```text
//! statement = DOMAIN ‖ version(1) ‖ statement_id(16) ‖ action(1)
//!             ‖ target_kind(1) ‖ target ‖ reason_commitment(32)
//!             ‖ until_ms(8, 0 = no end) ‖ issued_at_ms(8)
//!             ‖ moderator_id(16) ‖ list_version(8)
//! challenge = SHA-256(CHALLENGE_DOMAIN ‖ statement)
//! signed    = SIGNED_DOMAIN ‖ version(1) ‖ statement_len(2) ‖ statement
//!             ‖ authenticator_data_len(2) ‖ authenticator_data
//!             ‖ client_data_len(2) ‖ client_data ‖ signature_len(1) ‖ signature
//! ```
//!
//! The statement carries the reason only as a commitment,
//! `BLAKE3-derive-key(REASON_CONTEXT, reason ‖ salt(16))`: the statement is
//! what the public log holds, and the reason stays out of it. The applying
//! service is handed the reason and the salt beside the statement and checks
//! them with [`Statement::reason_matches`].
//!
//! Replay: a statement is applied once. The applying service keeps the
//! `statement_id`s it applied and refuses a repeat; that state is the
//! service's.
//!
//! [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md

use lakko_crypto::hash::{blake3_derive_key_from_material, sha256};

use crate::moderators::ModeratorList;
use crate::reason::Reason;
use crate::seal::MODERATOR_ID_LEN;
use crate::target::{Target, TargetKind};
use crate::webauthn::{
    verify_assertion, Assertion, MAX_AUTHENTICATOR_DATA, MAX_CLIENT_DATA, MAX_SIGNATURE,
};
use crate::wire::{header, put16, put8};
use crate::ModerationError;

/// The statement's domain separator. Frozen.
pub const STATEMENT_DOMAIN: &[u8] = b"lakko/moderation/statement/v1\0";
/// The challenge's domain separator. Frozen.
pub const CHALLENGE_DOMAIN: &[u8] = b"lakko/moderation/action/v1\0";
/// The signed statement's domain separator. Frozen.
pub const SIGNED_DOMAIN: &[u8] = b"lakko/moderation/signed-statement/v1\0";
/// The reason commitment's context. Frozen.
pub const REASON_CONTEXT: &str = "lakko/moderation/reason-commitment/v1";
/// The only version byte.
pub const STATEMENT_VERSION: u8 = 0x01;
/// The longest encoded statement.
pub const MAX_STATEMENT: usize = 512;

/// What a moderator does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Action {
    /// Suspend an account, until a time or with no end.
    Suspend = 1,
    /// Lift an account's suspension.
    Lift = 2,
    /// Take a username back and hold it.
    TakeBackName = 3,
    /// Release a held username.
    ReleaseName = 4,
    /// Remove an account from search.
    HideFromSearch = 5,
    /// Put an account back into search.
    ShowInSearch = 6,
    /// Remove a channel from Discover.
    ChannelDelist = 7,
    /// Delist a channel and refuse its `@name`.
    ChannelBlockHandle = 8,
    /// Remove a channel: its routes answer that it was removed.
    ChannelRemove = 9,
    /// Undo a delist, block or removal of a channel.
    ChannelRestore = 10,
    /// Remove a group from search.
    GroupDelist = 11,
    /// Remove a group from search and refuse its `@name`.
    GroupBlockHandle = 12,
    /// Undo a group delist or block.
    GroupRestore = 13,
    /// Delete a stored blob.
    BlobDelete = 14,
    /// Keep an account's suspension: decline the appeal pending against it.
    /// Changes no standing; the snapshot ignores it.
    KeepSuspension = 15,
}

impl Action {
    /// Decode a wire byte.
    ///
    /// # Errors
    ///
    /// [`ModerationError::UnknownValue`].
    pub const fn from_u8(byte: u8) -> Result<Self, ModerationError> {
        Ok(match byte {
            1 => Self::Suspend,
            2 => Self::Lift,
            3 => Self::TakeBackName,
            4 => Self::ReleaseName,
            5 => Self::HideFromSearch,
            6 => Self::ShowInSearch,
            7 => Self::ChannelDelist,
            8 => Self::ChannelBlockHandle,
            9 => Self::ChannelRemove,
            10 => Self::ChannelRestore,
            11 => Self::GroupDelist,
            12 => Self::GroupBlockHandle,
            13 => Self::GroupRestore,
            14 => Self::BlobDelete,
            15 => Self::KeepSuspension,
            _ => return Err(ModerationError::UnknownValue("action")),
        })
    }

    /// A stable kebab name (the panel, logs, the offline tool).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Lift => "lift",
            Self::TakeBackName => "take-back-name",
            Self::ReleaseName => "release-name",
            Self::HideFromSearch => "hide-from-search",
            Self::ShowInSearch => "show-in-search",
            Self::ChannelDelist => "channel-delist",
            Self::ChannelBlockHandle => "channel-block-handle",
            Self::ChannelRemove => "channel-remove",
            Self::ChannelRestore => "channel-restore",
            Self::GroupDelist => "group-delist",
            Self::GroupBlockHandle => "group-block-handle",
            Self::GroupRestore => "group-restore",
            Self::BlobDelete => "blob-delete",
            Self::KeepSuspension => "keep-suspension",
        }
    }

    /// The action of a [`Self::name`].
    ///
    /// # Errors
    ///
    /// [`ModerationError::UnknownValue`].
    pub fn from_name(name: &str) -> Result<Self, ModerationError> {
        (1..=15u8)
            .filter_map(|b| Self::from_u8(b).ok())
            .find(|a| a.name() == name)
            .ok_or(ModerationError::UnknownValue("action"))
    }

    /// The one target kind this action applies to.
    #[must_use]
    pub const fn target_kind(self) -> TargetKind {
        match self {
            Self::Suspend
            | Self::Lift
            | Self::HideFromSearch
            | Self::ShowInSearch
            | Self::KeepSuspension => TargetKind::Account,
            Self::TakeBackName | Self::ReleaseName => TargetKind::Name,
            Self::ChannelDelist
            | Self::ChannelBlockHandle
            | Self::ChannelRemove
            | Self::ChannelRestore => TargetKind::Channel,
            Self::GroupDelist | Self::GroupBlockHandle | Self::GroupRestore => {
                TargetKind::GroupHandle
            },
            Self::BlobDelete => TargetKind::Blob,
        }
    }
}

/// The reason commitment for `reason` under `salt`.
#[must_use]
pub fn reason_commitment(reason: Reason, salt: &[u8; 16]) -> [u8; 32] {
    let mut material = [0u8; 17];
    material[0] = reason.as_u8();
    material[1..].copy_from_slice(salt);
    blake3_derive_key_from_material(REASON_CONTEXT, &material)
}

/// A moderator's action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// A random id; an applying service applies an id once.
    pub statement_id: [u8; 16],
    /// The action.
    pub action: Action,
    /// What it applies to; its kind is the action's.
    pub target: Target,
    /// [`reason_commitment`] of the reason and a fresh salt.
    pub reason_commitment: [u8; 32],
    /// For [`Action::Suspend`]: the end (unix ms), or `0` for no end. `0`
    /// for every other action.
    pub until_ms: u64,
    /// When the moderator signed it (unix ms).
    pub issued_at_ms: u64,
    /// Who signed it.
    pub moderator_id: [u8; MODERATOR_ID_LEN],
    /// The moderator-list version it was made under.
    pub list_version: u64,
}

impl Statement {
    fn check(&self) -> Result<(), ModerationError> {
        if self.target.kind() != self.action.target_kind() {
            return Err(ModerationError::Invalid("target kind for action"));
        }
        if self.action != Action::Suspend && self.until_ms != 0 {
            return Err(ModerationError::Invalid("until on a non-suspension"));
        }
        if self.action == Action::Suspend
            && self.until_ms != 0
            && self.until_ms <= self.issued_at_ms
        {
            return Err(ModerationError::Invalid("until before issue"));
        }
        Ok(())
    }

    /// Encode.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        out.extend_from_slice(STATEMENT_DOMAIN);
        out.push(STATEMENT_VERSION);
        out.extend_from_slice(&self.statement_id);
        out.push(self.action as u8);
        self.target.write(&mut out);
        out.extend_from_slice(&self.reason_commitment);
        out.extend_from_slice(&self.until_ms.to_be_bytes());
        out.extend_from_slice(&self.issued_at_ms.to_be_bytes());
        out.extend_from_slice(&self.moderator_id);
        out.extend_from_slice(&self.list_version.to_be_bytes());
        out
    }

    /// Encode, refusing a statement [`Self::decode`] would refuse.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`].
    pub fn encode_checked(&self) -> Result<Vec<u8>, ModerationError> {
        self.check()?;
        Ok(self.encode())
    }

    /// Decode, exact.
    ///
    /// # Errors
    ///
    /// Any decode variant, or [`ModerationError::Invalid`].
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        let mut r = header(bytes, STATEMENT_DOMAIN, STATEMENT_VERSION)?;
        let s = Self {
            statement_id: r.array()?,
            action: Action::from_u8(r.u8()?)?,
            target: Target::read(&mut r)?,
            reason_commitment: r.array()?,
            until_ms: r.u64()?,
            issued_at_ms: r.u64()?,
            moderator_id: r.array()?,
            list_version: r.u64()?,
        };
        r.finish()?;
        s.check()?;
        Ok(s)
    }

    /// The 32-byte WebAuthn challenge of this statement.
    #[must_use]
    pub fn challenge(&self) -> [u8; 32] {
        let mut input = CHALLENGE_DOMAIN.to_vec();
        input.extend_from_slice(&self.encode());
        sha256(&input)
    }

    /// Whether `reason` and `salt` open this statement's commitment.
    #[must_use]
    pub fn reason_matches(&self, reason: Reason, salt: &[u8; 16]) -> bool {
        reason_commitment(reason, salt) == self.reason_commitment
    }
}

/// A statement with the moderator's WebAuthn assertion over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedStatement {
    /// The encoded statement.
    pub statement: Vec<u8>,
    /// The assertion.
    pub assertion: Assertion,
}

impl SignedStatement {
    /// Encode.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`] for a field over its bound.
    pub fn encode(&self) -> Result<Vec<u8>, ModerationError> {
        let mut out = Vec::new();
        out.extend_from_slice(SIGNED_DOMAIN);
        out.push(STATEMENT_VERSION);
        put16(&mut out, &self.statement, MAX_STATEMENT)?;
        put16(
            &mut out,
            &self.assertion.authenticator_data,
            MAX_AUTHENTICATOR_DATA,
        )?;
        put16(&mut out, &self.assertion.client_data_json, MAX_CLIENT_DATA)?;
        put8(&mut out, &self.assertion.signature, MAX_SIGNATURE)?;
        Ok(out)
    }

    /// Decode, exact. Does not verify.
    ///
    /// # Errors
    ///
    /// Any decode variant.
    pub fn decode(bytes: &[u8]) -> Result<Self, ModerationError> {
        let mut r = header(bytes, SIGNED_DOMAIN, STATEMENT_VERSION)?;
        let statement = r.bytes16(MAX_STATEMENT)?.to_vec();
        let assertion = Assertion {
            authenticator_data: r.bytes16(MAX_AUTHENTICATOR_DATA)?.to_vec(),
            client_data_json: r.bytes16(MAX_CLIENT_DATA)?.to_vec(),
            signature: r.bytes8(MAX_SIGNATURE)?.to_vec(),
        };
        r.finish()?;
        Ok(Self {
            statement,
            assertion,
        })
    }

    /// Verify against a moderator list the caller has already verified
    /// under the pinned root, and return the statement.
    ///
    /// The list must be at least as new as the one the statement names, and
    /// the moderator must have been listed and not revoked when it signed.
    ///
    /// # Errors
    ///
    /// Decode errors, [`ModerationError::NotAuthorised`],
    /// [`ModerationError::Assertion`], [`ModerationError::BadSignature`].
    pub fn verify(&self, list: &ModeratorList) -> Result<Statement, ModerationError> {
        let statement = Statement::decode(&self.statement)?;
        if list.list_version < statement.list_version {
            return Err(ModerationError::NotAuthorised);
        }
        let entry = list
            .authorised_at(&statement.moderator_id, statement.issued_at_ms)
            .ok_or(ModerationError::NotAuthorised)?;
        verify_assertion(list, &entry.key, &statement.challenge(), &self.assertion)?;
        Ok(statement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moderators::tests::{entry, list};
    use crate::moderators::ModeratorKey;
    use crate::webauthn::tests::{keys, make};
    use proptest::prelude::*;

    fn statement(moderator: u8) -> Statement {
        Statement {
            statement_id: [1; 16],
            action: Action::Suspend,
            target: Target::account(&[7; 32]).unwrap(),
            reason_commitment: reason_commitment(Reason::Spam, &[3; 16]),
            until_ms: 10_000,
            issued_at_ms: 5_000,
            moderator_id: [moderator; 16],
            list_version: 3,
        }
    }

    fn list_with_keys() -> ModeratorList {
        let [ed, p] = keys();
        let mut l = list();
        l.entries = vec![entry(1, ed), entry(2, p)];
        l
    }

    fn signed(s: &Statement, key: &ModeratorKey) -> SignedStatement {
        SignedStatement {
            statement: s.encode(),
            assertion: make(&list_with_keys(), key, &s.challenge(), 0x05),
        }
    }

    #[test]
    fn a_listed_moderator_signs_and_it_verifies() {
        let l = list_with_keys();
        let [ed, p] = keys();
        for (n, key) in [(1u8, ed), (2u8, p)] {
            let s = statement(n);
            let wire = signed(&s, &key).encode().unwrap();
            let back = SignedStatement::decode(&wire).unwrap();
            assert_eq!(back.verify(&l).unwrap(), s);
        }
    }

    #[test]
    fn another_moderators_signature_is_refused() {
        let l = list_with_keys();
        let [ed, _] = keys();
        let s = statement(2);
        assert!(signed(&s, &ed).verify(&l).is_err());
    }

    #[test]
    fn revoked_unknown_and_newer_list_are_refused() {
        let [ed, _] = keys();
        let s = statement(1);
        let ss = signed(&s, &ed);
        let mut l = list_with_keys();
        l.entries[0].revoked_at_ms = 5_000;
        assert_eq!(ss.verify(&l), Err(ModerationError::NotAuthorised));
        let mut l = list_with_keys();
        l.list_version = 2;
        assert_eq!(ss.verify(&l), Err(ModerationError::NotAuthorised));
        let mut s9 = statement(9);
        s9.statement_id = [2; 16];
        assert_eq!(
            signed(&s9, &ed).verify(&list_with_keys()),
            Err(ModerationError::NotAuthorised)
        );
    }

    #[test]
    fn a_changed_statement_does_not_verify() {
        let l = list_with_keys();
        let [ed, _] = keys();
        let s = statement(1);
        let mut ss = signed(&s, &ed);
        let mut other = s.clone();
        other.until_ms = 0;
        ss.statement = other.encode();
        assert_eq!(ss.verify(&l), Err(ModerationError::Assertion("challenge")));
    }

    #[test]
    fn rules_of_the_statement() {
        let mut s = statement(1);
        s.action = Action::ChannelRemove;
        assert!(s.encode_checked().is_err());
        assert!(Statement::decode(&s.encode()).is_err());
        let mut s = statement(1);
        s.action = Action::Lift;
        assert!(s.encode_checked().is_err(), "until on a lift");
        let mut s = statement(1);
        s.until_ms = 4_000;
        assert!(s.encode_checked().is_err());
        let s = statement(1);
        assert!(s.reason_matches(Reason::Spam, &[3; 16]));
        assert!(!s.reason_matches(Reason::Hate, &[3; 16]));
        assert!(!s.reason_matches(Reason::Spam, &[4; 16]));
    }

    #[test]
    fn every_action_byte_round_trips() {
        let decoded: Vec<u8> = (0..=u8::MAX)
            .filter(|b| Action::from_u8(*b).is_ok())
            .collect();
        assert_eq!(decoded, (1..=15).collect::<Vec<u8>>());
        for b in decoded {
            let action = Action::from_u8(b).unwrap();
            assert_eq!(Action::from_name(action.name()), Ok(action));
        }
        assert!(Action::from_name("Suspend").is_err());
    }

    proptest! {
        #[test]
        fn decoders_are_exact(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(s) = Statement::decode(&bytes) {
                prop_assert_eq!(s.encode(), bytes.clone());
            }
            if let Ok(s) = SignedStatement::decode(&bytes) {
                prop_assert_eq!(s.encode().unwrap(), bytes.clone());
            }
        }
    }
}
