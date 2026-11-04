// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The epoch key and the chain of older ones (ADR-0086 section 3).
//!
//! An epoch key is what a member holds for one epoch: it derives the read
//! key, and it wraps row keys and seals the filter written while it was
//! current. A removal moves the epoch; nothing already stored moves, so a row
//! keeps the epoch it was written in.
//!
//! The **chain** is what lets a contact who joined later open those older
//! rows. Each new epoch key wraps the previous one, in the public header:
//!
//! ```text
//! chain[e] = nonce || ChaCha20-Poly1305(chain key of epoch e, aad, epoch key of e - 1)
//! aad      = "lakko/circle/chain-wrap/v1\0" || directory_id || e(u64 BE)
//! ```
//!
//! The chain only runs downward. Holding an epoch key gives every earlier
//! one and no later one, which is what makes a removal final: a removed
//! contact keeps what he could read when he left, and a key that opens
//! nothing written after it.

use lakko_crypto::signing::SigningKey;
use lakko_crypto::SecretBytes;

use crate::error::CircleError;
use crate::keys::{
    derive, open_wrap, seal_wrap, CHAIN_CONTEXT, DIRECTORY_ID_LEN, EPOCH_CONTEXT,
    FILTER_HASH_CONTEXT, FILTER_SEAL_CONTEXT, KEY_LEN, READ_CONTEXT, ROW_WRAP_CONTEXT, WRAP_LEN,
};

const CHAIN_AAD_DOMAIN: &[u8] = b"lakko/circle/chain-wrap/v1\0";

/// The key of one epoch of one directory. Prints nothing in `Debug`, compares
/// in constant time, is zeroed on drop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochKey {
    directory_id: [u8; DIRECTORY_ID_LEN],
    epoch: u64,
    key: SecretBytes<KEY_LEN>,
}

impl EpochKey {
    /// The epoch key the tree's root key gives at `epoch`.
    #[must_use]
    pub(crate) fn from_root(
        directory_id: [u8; DIRECTORY_ID_LEN],
        epoch: u64,
        root: &SecretBytes<KEY_LEN>,
    ) -> Self {
        let mut material = [0u8; KEY_LEN + 8];
        let (root_part, epoch_part) = material.split_at_mut(KEY_LEN);
        root_part.copy_from_slice(root.as_bytes());
        epoch_part.copy_from_slice(&epoch.to_be_bytes());
        let scratch = SecretBytes::new(material);
        Self {
            directory_id,
            epoch,
            key: derive(EPOCH_CONTEXT, scratch.as_bytes()),
        }
    }

    /// The directory this key belongs to.
    #[must_use]
    pub const fn directory_id(&self) -> &[u8; DIRECTORY_ID_LEN] {
        &self.directory_id
    }

    /// The epoch this key belongs to.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The Ed25519 seed of this epoch's read key.
    #[must_use]
    pub fn read_seed(&self) -> SecretBytes<KEY_LEN> {
        derive(READ_CONTEXT, self.key.as_bytes())
    }

    /// The public half of the read key: what the directory registers, and
    /// what a member compares its derivation against.
    #[must_use]
    pub fn read_public_key(&self) -> [u8; KEY_LEN] {
        SigningKey::from_seed(self.read_seed().as_bytes())
            .verifying_key()
            .to_bytes()
    }

    pub(crate) fn row_wrap_key(&self) -> SecretBytes<KEY_LEN> {
        derive(ROW_WRAP_CONTEXT, self.key.as_bytes())
    }

    pub(crate) fn filter_seal_key(&self) -> SecretBytes<KEY_LEN> {
        derive(FILTER_SEAL_CONTEXT, self.key.as_bytes())
    }

    pub(crate) fn filter_hash_key(&self) -> SecretBytes<KEY_LEN> {
        derive(FILTER_HASH_CONTEXT, self.key.as_bytes())
    }

    fn chain_aad(directory_id: &[u8; DIRECTORY_ID_LEN], epoch: u64) -> Vec<u8> {
        let mut aad = Vec::with_capacity(CHAIN_AAD_DOMAIN.len() + DIRECTORY_ID_LEN + 8);
        aad.extend_from_slice(CHAIN_AAD_DOMAIN);
        aad.extend_from_slice(directory_id);
        aad.extend_from_slice(&epoch.to_be_bytes());
        aad
    }

    /// The chain entry of this epoch: `previous` wrapped under this key.
    ///
    /// # Errors
    ///
    /// [`CircleError::Malformed`] when `previous` is not the epoch before
    /// this one of the same directory, [`CircleError::Seal`] on a cipher
    /// failure.
    pub fn wrap_previous(&self, previous: &Self) -> Result<[u8; WRAP_LEN], CircleError> {
        let expected = self.epoch.checked_sub(1).ok_or(CircleError::Malformed)?;
        if previous.epoch != expected || previous.directory_id != self.directory_id {
            return Err(CircleError::Malformed);
        }
        seal_wrap(
            &derive(CHAIN_CONTEXT, self.key.as_bytes()),
            &Self::chain_aad(&self.directory_id, self.epoch),
            &previous.key,
        )
    }

    /// The epoch key before this one, from this epoch's chain entry.
    ///
    /// # Errors
    ///
    /// [`CircleError::Authentication`] when the entry was not made under this
    /// key, for this epoch of this directory; [`CircleError::Malformed`] at
    /// epoch 0.
    pub fn previous(&self, wrap: &[u8; WRAP_LEN]) -> Result<Self, CircleError> {
        let epoch = self.epoch.checked_sub(1).ok_or(CircleError::Malformed)?;
        let key = open_wrap(
            &derive(CHAIN_CONTEXT, self.key.as_bytes()),
            &Self::chain_aad(&self.directory_id, self.epoch),
            wrap,
        )?;
        Ok(Self {
            directory_id: self.directory_id,
            epoch,
            key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::hex;

    fn key_at(epoch: u8) -> EpochKey {
        EpochKey::from_root(
            [7u8; 32],
            u64::from(epoch),
            &SecretBytes::new([epoch; KEY_LEN]),
        )
    }

    #[test]
    fn the_derivations_below_an_epoch_key_are_pinned() {
        let key = EpochKey::from_root([7u8; 32], 3, &SecretBytes::new([9u8; KEY_LEN]));
        assert_eq!(
            hex(key.key.as_bytes()),
            "aeb033ae55cd75028cc96637cc664ba01996d06fd7c2c56ab7bf17846319ea32"
        );
        assert_eq!(
            hex(key.read_seed().as_bytes()),
            "e1067e803f955d1139d611c8608f4cf70821437a54fd8de4bc22c6ada3f981db"
        );
        assert_eq!(
            hex(&key.read_public_key()),
            "36bd224f783d7774edcb318ef1ae3265a89794f2fa49946931465dc8dc8b6ff3"
        );
        assert_eq!(
            hex(key.row_wrap_key().as_bytes()),
            "1959fe31d3b834092b7481272add5a4003e46aaabc6128ad2f3b4ca40138a28a"
        );
        assert_eq!(
            hex(key.filter_seal_key().as_bytes()),
            "253436bb2dc0d89e800bd7d2dae2fb15c25e64f786b816f88972a46666525429"
        );
        assert_eq!(
            hex(key.filter_hash_key().as_bytes()),
            "0c77f9ee86ab1d86ebfb089c2e0a08b8bb52d72ea37309f9447a056d848d2a93"
        );
    }

    #[test]
    fn the_epoch_number_enters_the_key() {
        let root = SecretBytes::new([9u8; KEY_LEN]);
        assert_ne!(
            EpochKey::from_root([7u8; 32], 3, &root),
            EpochKey::from_root([7u8; 32], 4, &root)
        );
    }

    #[test]
    fn the_chain_runs_down_only() {
        let older = key_at(4);
        let newer = key_at(5);
        let wrap = newer.wrap_previous(&older).unwrap();
        assert_eq!(newer.previous(&wrap).unwrap(), older);
        // The older key does not open the entry that names it.
        assert_eq!(older.previous(&wrap), Err(CircleError::Authentication));
    }

    #[test]
    fn a_chain_entry_is_bound_to_its_epoch_and_directory() {
        let newer = key_at(5);
        let wrap = newer.wrap_previous(&key_at(4)).unwrap();
        let moved = EpochKey {
            epoch: 6,
            ..newer.clone()
        };
        assert_eq!(moved.previous(&wrap), Err(CircleError::Authentication));
        let elsewhere = EpochKey {
            directory_id: [8u8; 32],
            ..newer.clone()
        };
        assert_eq!(elsewhere.previous(&wrap), Err(CircleError::Authentication));
    }

    #[test]
    fn wrap_previous_refuses_a_key_that_is_not_the_one_before() {
        assert_eq!(
            key_at(5).wrap_previous(&key_at(3)),
            Err(CircleError::Malformed)
        );
    }
}
