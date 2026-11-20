// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What a report or an action is about ([ADR-0099] sections 2 and 4).
//!
//! | Kind | Bytes | What they are |
//! |---|---|---|
//! | account | 32 | `BLAKE3-derive-key("lakko/moderation/account-target/v1", account_key)` |
//! | group handle | 64 | the `@name`'s OPRF lookup key, as discovery stores it |
//! | channel | 16 | the channel id |
//! | name | 64 | a username's OPRF lookup key |
//! | blob | 32 | a blob id (SHA-256 of its ciphertext) |
//!
//! An account is named by a hash of its key rather than the key, so the
//! public log does not carry the key itself; auth finds the account from the
//! key it is handed beside the statement and checks it hashes to the target.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use lakko_crypto::hash::blake3_derive_key_from_material;

use crate::wire::Reader;
use crate::ModerationError;

/// The context of the account-target hash. Frozen.
pub const ACCOUNT_TARGET_CONTEXT: &str = "lakko/moderation/account-target/v1";

/// The kind of thing a target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum TargetKind {
    /// An account.
    Account = 1,
    /// A group's `@name`.
    GroupHandle = 2,
    /// A channel.
    Channel = 3,
    /// A username.
    Name = 4,
    /// A stored blob.
    Blob = 5,
}

impl TargetKind {
    /// The wire byte.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte.
    ///
    /// # Errors
    ///
    /// [`ModerationError::UnknownValue`].
    pub const fn from_u8(byte: u8) -> Result<Self, ModerationError> {
        Ok(match byte {
            1 => Self::Account,
            2 => Self::GroupHandle,
            3 => Self::Channel,
            4 => Self::Name,
            5 => Self::Blob,
            _ => return Err(ModerationError::UnknownValue("target kind")),
        })
    }

    /// The exact length of a target of this kind.
    #[must_use]
    pub const fn target_len(self) -> usize {
        match self {
            Self::Account | Self::Blob => 32,
            Self::GroupHandle | Self::Name => 64,
            Self::Channel => 16,
        }
    }
}

/// A target: its kind and its bytes, of exactly the kind's length.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    kind: TargetKind,
    bytes: Vec<u8>,
}

impl Target {
    /// Build a target; the bytes must be the kind's length.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`].
    pub fn new(kind: TargetKind, bytes: &[u8]) -> Result<Self, ModerationError> {
        if bytes.len() != kind.target_len() {
            return Err(ModerationError::Invalid("target length"));
        }
        Ok(Self {
            kind,
            bytes: bytes.to_vec(),
        })
    }

    /// The target of an account, from its 32-byte account key.
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`] for a key that is not 32 bytes.
    pub fn account(account_key: &[u8]) -> Result<Self, ModerationError> {
        if account_key.len() != 32 {
            return Err(ModerationError::Invalid("account key length"));
        }
        Ok(Self {
            kind: TargetKind::Account,
            bytes: account_target_hash(account_key).to_vec(),
        })
    }

    /// The kind.
    #[must_use]
    pub const fn kind(&self) -> TargetKind {
        self.kind
    }

    /// The bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.push(self.kind.as_u8());
        out.extend_from_slice(&self.bytes);
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, ModerationError> {
        let kind = TargetKind::from_u8(r.u8()?)?;
        let bytes = r.take(kind.target_len())?.to_vec();
        Ok(Self { kind, bytes })
    }
}

/// The 32-byte hash that names an account in reports, statements and the log.
#[must_use]
pub fn account_target_hash(account_key: &[u8]) -> [u8; 32] {
    blake3_derive_key_from_material(ACCOUNT_TARGET_CONTEXT, account_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_enforced() {
        for kind in [
            TargetKind::Account,
            TargetKind::GroupHandle,
            TargetKind::Channel,
            TargetKind::Name,
            TargetKind::Blob,
        ] {
            assert!(Target::new(kind, &vec![0u8; kind.target_len()]).is_ok());
            assert!(Target::new(kind, &vec![0u8; kind.target_len() + 1]).is_err());
            assert_eq!(TargetKind::from_u8(kind.as_u8()), Ok(kind));
        }
        assert!(TargetKind::from_u8(0).is_err());
        assert!(TargetKind::from_u8(6).is_err());
    }

    #[test]
    fn the_account_target_is_a_keyed_hash_not_the_key() {
        let key = [3u8; 32];
        let t = Target::account(&key).unwrap();
        assert_ne!(t.bytes(), key.as_slice());
        assert_eq!(t.bytes(), account_target_hash(&key).as_slice());
        assert_ne!(account_target_hash(&key), account_target_hash(&[4u8; 32]));
    }
}
