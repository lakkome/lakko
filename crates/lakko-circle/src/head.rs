// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The directory head and the member grant (ADR-0086 sections 2 and 3).
//!
//! The **head** is the small public part of a directory: what a contact reads
//! to learn whether anything moved. It is readable by anyone holding the
//! directory id, carries no secret, and the Delivery Service stores it
//! without reading it.
//!
//! ```text
//! head := format(1) = 0x01 || epoch(8) || version(8) || filter_version(8)
//!         || read_pk(32) || capacity_log(1)
//! ```
//!
//! `epoch` moves on a removal, `version` on every write, `filter_version` when
//! a new filter is written. `capacity_log` says how many of a leaf's path
//! nodes exist ([`crate::tree`]). `read_pk` is what a member compares its
//! derivation against, so a stale or incomplete path is noticed before
//! anything is sent.
//!
//! The **grant** is what the owner gives a contact once, in their one-to-one
//! conversation: the directory and one leaf key.
//!
//! ```text
//! grant := format(1) = 0x01 || directory_id(32) || leaf(4) || leaf_key(32)
//! ```

use lakko_crypto::SecretBytes;

use crate::codec::Reader;
use crate::error::CircleError;
use crate::keys::{DIRECTORY_ID_LEN, KEY_LEN};
use crate::tree::{MAX_CAPACITY_LOG, MAX_EPOCH, MAX_LEAVES, MIN_CAPACITY_LOG};

const FORMAT_V1: u8 = 0x01;

/// Length of an encoded head.
pub const HEAD_LEN: usize = 1 + 8 + 8 + 8 + KEY_LEN + 1;
/// Length of an encoded grant.
pub const GRANT_LEN: usize = 1 + DIRECTORY_ID_LEN + 4 + KEY_LEN;

/// The public head of a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryHead {
    /// The current epoch.
    pub epoch: u64,
    /// The version of the directory: one more with every write.
    pub version: u64,
    /// The version of the current filter; 0 until one is written.
    pub filter_version: u64,
    /// The read key of the current epoch.
    pub read_pk: [u8; KEY_LEN],
    /// The tree holds `1 << capacity_log` leaves.
    pub capacity_log: u8,
}

impl DirectoryHead {
    /// The wire form.
    #[must_use]
    pub fn encode(&self) -> [u8; HEAD_LEN] {
        let mut out = [0u8; HEAD_LEN];
        out[0] = FORMAT_V1;
        out[1..9].copy_from_slice(&self.epoch.to_be_bytes());
        out[9..17].copy_from_slice(&self.version.to_be_bytes());
        out[17..25].copy_from_slice(&self.filter_version.to_be_bytes());
        out[25..57].copy_from_slice(&self.read_pk);
        out[57] = self.capacity_log;
        out
    }

    /// Decode a head, fail-closed.
    ///
    /// # Errors
    ///
    /// [`CircleError::Length`], [`CircleError::UnknownFormat`], or
    /// [`CircleError::Malformed`] for an epoch of 0 or a capacity out of
    /// range.
    pub fn decode(bytes: &[u8]) -> Result<Self, CircleError> {
        if bytes.len() != HEAD_LEN {
            return Err(CircleError::Length);
        }
        let mut r = Reader::new(bytes);
        if r.u8()? != FORMAT_V1 {
            return Err(CircleError::UnknownFormat);
        }
        let epoch = r.u64()?;
        let version = r.u64()?;
        let filter_version = r.u64()?;
        let read_pk = r.array()?;
        let capacity_log = r.u8()?;
        r.finish()?;
        if epoch == 0
            || epoch > MAX_EPOCH
            || !(MIN_CAPACITY_LOG..=MAX_CAPACITY_LOG).contains(&capacity_log)
        {
            return Err(CircleError::Malformed);
        }
        Ok(Self {
            epoch,
            version,
            filter_version,
            read_pk,
            capacity_log,
        })
    }
}

/// What a contact holds: the directory and one leaf key. Prints no key.
#[derive(Clone, PartialEq, Eq)]
pub struct MemberGrant {
    /// The directory this grant opens.
    pub directory_id: [u8; DIRECTORY_ID_LEN],
    /// The contact's leaf.
    pub leaf: u32,
    /// The contact's leaf key.
    pub leaf_key: SecretBytes<KEY_LEN>,
}

impl core::fmt::Debug for MemberGrant {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemberGrant")
            .field("leaf", &self.leaf)
            .finish_non_exhaustive()
    }
}

impl MemberGrant {
    /// The wire form. Secret: it travels sealed, in a one-to-one conversation.
    #[must_use]
    pub fn encode(&self) -> [u8; GRANT_LEN] {
        let mut out = [0u8; GRANT_LEN];
        out[0] = FORMAT_V1;
        out[1..33].copy_from_slice(&self.directory_id);
        out[33..37].copy_from_slice(&self.leaf.to_be_bytes());
        out[37..].copy_from_slice(self.leaf_key.as_bytes());
        out
    }

    /// Decode a grant, fail-closed.
    ///
    /// # Errors
    ///
    /// [`CircleError::Length`], [`CircleError::UnknownFormat`], or
    /// [`CircleError::Malformed`] for a leaf outside the tree.
    pub fn decode(bytes: &[u8]) -> Result<Self, CircleError> {
        if bytes.len() != GRANT_LEN {
            return Err(CircleError::Length);
        }
        let mut r = Reader::new(bytes);
        if r.u8()? != FORMAT_V1 {
            return Err(CircleError::UnknownFormat);
        }
        let directory_id = r.array()?;
        let leaf = r.u32()?;
        let leaf_key = SecretBytes::new(r.array()?);
        r.finish()?;
        if leaf >= MAX_LEAVES {
            return Err(CircleError::Malformed);
        }
        Ok(Self {
            directory_id,
            leaf,
            leaf_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> DirectoryHead {
        DirectoryHead {
            epoch: 3,
            version: 9,
            filter_version: 2,
            read_pk: [7; KEY_LEN],
            capacity_log: 8,
        }
    }

    #[test]
    fn a_head_round_trips() {
        assert_eq!(DirectoryHead::decode(&head().encode()), Ok(head()));
        assert_eq!(head().encode().len(), 58);
    }

    #[test]
    fn a_head_is_refused_on_every_bound() {
        let good = head().encode();
        assert_eq!(DirectoryHead::decode(&good[..57]), Err(CircleError::Length));
        let mut format = good;
        format[0] = 2;
        assert_eq!(
            DirectoryHead::decode(&format),
            Err(CircleError::UnknownFormat)
        );
        let mut capacity = good;
        capacity[57] = 3;
        assert_eq!(
            DirectoryHead::decode(&capacity),
            Err(CircleError::Malformed)
        );
        capacity[57] = 17;
        assert_eq!(
            DirectoryHead::decode(&capacity),
            Err(CircleError::Malformed)
        );
        let mut epoch = head();
        epoch.epoch = 0;
        assert_eq!(
            DirectoryHead::decode(&epoch.encode()),
            Err(CircleError::Malformed)
        );
    }

    #[test]
    fn a_grant_round_trips_and_is_bounded() {
        let grant = MemberGrant {
            directory_id: [1; 32],
            leaf: 65_535,
            leaf_key: SecretBytes::new([2; 32]),
        };
        assert_eq!(MemberGrant::decode(&grant.encode()), Ok(grant.clone()));
        assert_eq!(grant.encode().len(), 69);
        let mut too_far = grant;
        too_far.leaf = MAX_LEAVES;
        assert_eq!(
            MemberGrant::decode(&too_far.encode()),
            Err(CircleError::Malformed)
        );
    }

    #[test]
    fn a_grant_prints_no_key() {
        let grant = MemberGrant {
            directory_id: [1; 32],
            leaf: 5,
            leaf_key: SecretBytes::new([0xAB; 32]),
        };
        assert!(!format!("{grant:?}").to_lowercase().contains("ab, "));
    }
}
