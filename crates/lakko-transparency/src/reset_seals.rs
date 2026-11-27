// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The seal-announcement leaf ([ADR-0101] section 10).
//!
//! An account's seal announcement (`lakko_attest::SealAnnouncement`) is
//! appended to the KT log as a leaf of its own kind for the account's
//! `username_hash`, so its history is witnessed like the identity bindings
//! (ADR-0039). A contact's device checking a reset (ADR-0073 R.2 rule 4)
//! asks the log the KT-log half of the question: is the announcement it was
//! served the one the log holds for generation `g-1`, before the reset
//! leaf?
//!
//! ## The leaf
//!
//! ```text
//! leaf = "lakko/transparency/reset-seals-leaf/v1\0"
//!        ‖ username_hash(32) ‖ key_generation(8 BE) ‖ seq(8 BE)
//!        ‖ announcement_hash(32)
//! announcement_hash = BLAKE3(announcement bytes as served)
//! ```
//!
//! 119 bytes. **Chosen 2026-11-26 (not in the ADR):** the leaf commits the
//! announcement by its BLAKE3 hash, not the up-to-641 bytes themselves, and
//! opens with a domain separator: an identity leaf
//! ([`crate::LogEntry::canonical_bytes`]) opens with a 32-byte username hash,
//! so the two kinds cannot be the same bytes unless that hash equals the
//! ASCII separator, and their lengths differ for every identity key length
//! but one.
//!
//! ## What the check proves, and what it does not
//!
//! [`verify_announcement_before_reset`] proves that the served announcement
//! is committed by a root the caller already verified (operator signature,
//! no-rollback, and the witness quorum when pinned) at a leaf index below
//! the reset leaf's, for this account and generation. "The newest" is what
//! the log service serves as the newest announcement of `g-1`: a Merkle
//! inclusion proof shows presence, not the absence of a later leaf, so
//! "newest" rests on the operator answering truthfully, kept honest by the
//! witnesses against a split view and by the `seq` floor each device keeps
//! (an older announcement served after a newer one was seen is refused by
//! `lakko_attest::check_reset_seal`). Rolling back to an older announcement
//! a device never saw still needs a seal key from it (an older password or
//! a removed passkey): the stated residual of ADR-0101 section 10.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use lakko_crypto::hash;

use crate::error::{Error, Result};
use crate::log::{InclusionProof, SignedRoot};
use crate::merkle;

/// Domain separator opening every announcement leaf. Frozen.
pub const ANNOUNCEMENT_LEAF_DOMAIN: &[u8] = b"lakko/transparency/reset-seals-leaf/v1\0";

/// Length of an announcement leaf: 119 bytes.
pub const ANNOUNCEMENT_LEAF_LEN: usize = ANNOUNCEMENT_LEAF_DOMAIN.len() + 32 + 8 + 8 + 32;

const _: () = assert!(ANNOUNCEMENT_LEAF_LEN == 119);

/// One announcement leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnouncementLeaf {
    /// `account_username_hash(account_id)`.
    pub username_hash: [u8; 32],
    /// The generation the announcement was made at.
    pub key_generation: u64,
    /// The announcement's `seq`.
    pub seq: u64,
    /// `BLAKE3` of the announcement bytes.
    pub announcement_hash: [u8; 32],
}

impl AnnouncementLeaf {
    /// The leaf for `announcement`, as served.
    #[must_use]
    pub fn for_announcement(
        username_hash: [u8; 32],
        key_generation: u64,
        seq: u64,
        announcement: &[u8],
    ) -> Self {
        Self {
            username_hash,
            key_generation,
            seq,
            announcement_hash: hash::blake3(announcement),
        }
    }

    /// The leaf bytes the Merkle tree hashes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ANNOUNCEMENT_LEAF_LEN);
        out.extend_from_slice(ANNOUNCEMENT_LEAF_DOMAIN);
        out.extend_from_slice(&self.username_hash);
        out.extend_from_slice(&self.key_generation.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.announcement_hash);
        out
    }
}

/// The KT-log half of ADR-0073 R.2 rule 4: `leaf` (built with
/// [`AnnouncementLeaf::for_announcement`] from the served announcement and
/// the generation the device accepted) is included at `proof.leaf_index` of
/// `signed_root`, and that index is below `reset_leaf_index`, the index of
/// the reset's identity leaf in the same tree.
///
/// The caller verifies `signed_root` first (operator signature, the
/// no-rollback check, the witness quorum when pinned), and has verified the
/// reset leaf's own inclusion at `reset_leaf_index` against the same root.
///
/// # Errors
///
/// [`Error::IndexOutOfBounds`] when `reset_leaf_index` is not in the tree,
/// [`Error::AnnouncementNotBeforeReset`] when the announcement's index is
/// not below it, [`Error::InclusionProofMismatch`] when the proof does not
/// bind the leaf to the root.
pub fn verify_announcement_before_reset(
    leaf: &AnnouncementLeaf,
    proof: &InclusionProof,
    reset_leaf_index: u64,
    signed_root: &SignedRoot,
) -> Result<()> {
    if reset_leaf_index >= signed_root.log_size {
        return Err(Error::IndexOutOfBounds {
            index: reset_leaf_index,
            size: signed_root.log_size,
        });
    }
    if proof.leaf_index >= reset_leaf_index {
        return Err(Error::AnnouncementNotBeforeReset);
    }
    if merkle::verify_inclusion(
        &leaf.canonical_bytes(),
        proof.leaf_index,
        signed_root.log_size,
        &proof.audit_path,
        &signed_root.tree_root,
    ) {
        Ok(())
    } else {
        Err(Error::InclusionProofMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{InMemoryLog, LogEntry};
    use lakko_crypto::signing::SigningKey;

    fn tree(leaves: &[Vec<u8>]) -> (SignedRoot, SigningKey) {
        let operator = SigningKey::from_seed(&[0x01; 32]);
        let tree_root = merkle::root(leaves);
        let log_size = leaves.len() as u64;
        let signature = operator.sign(&SignedRoot::signing_payload(&tree_root, log_size, 7));
        (
            SignedRoot {
                tree_root,
                log_size,
                timestamp_unix: 7,
                signature,
            },
            operator,
        )
    }

    fn proof(leaves: &[Vec<u8>], index: usize) -> InclusionProof {
        InclusionProof {
            leaf_index: index as u64,
            audit_path: merkle::audit_path(leaves, index),
        }
    }

    const USER: [u8; 32] = [0x55; 32];

    fn scenario() -> (Vec<Vec<u8>>, AnnouncementLeaf) {
        let genesis = LogEntry {
            username_hash: USER,
            identity_pubkey: vec![0x0A; 32],
            version: 1,
        };
        let ann = AnnouncementLeaf::for_announcement(USER, 1, 3, b"announcement-g1");
        let reset = LogEntry {
            username_hash: USER,
            identity_pubkey: vec![0x0B; 32],
            version: 2,
        };
        let leaves = vec![
            genesis.canonical_bytes(),
            ann.canonical_bytes(),
            reset.canonical_bytes(),
            b"later".to_vec(),
        ];
        (leaves, ann)
    }

    #[test]
    fn an_announcement_before_the_reset_passes() {
        let (leaves, ann) = scenario();
        let (root, operator) = tree(&leaves);
        assert!(root.verify(&operator.verifying_key()).is_ok());
        assert!(verify_announcement_before_reset(&ann, &proof(&leaves, 1), 2, &root).is_ok());
    }

    #[test]
    fn every_failure_is_refused() {
        let (leaves, ann) = scenario();
        let (root, _) = tree(&leaves);
        // After the reset leaf, or the reset index itself.
        assert!(matches!(
            verify_announcement_before_reset(&ann, &proof(&leaves, 1), 1, &root),
            Err(Error::AnnouncementNotBeforeReset)
        ));
        assert!(matches!(
            verify_announcement_before_reset(&ann, &proof(&leaves, 1), 9, &root),
            Err(Error::IndexOutOfBounds { .. })
        ));
        // Another announcement, generation, seq or account than the leaf holds.
        for other in [
            AnnouncementLeaf::for_announcement(USER, 1, 3, b"another"),
            AnnouncementLeaf::for_announcement(USER, 2, 3, b"announcement-g1"),
            AnnouncementLeaf::for_announcement(USER, 1, 4, b"announcement-g1"),
            AnnouncementLeaf::for_announcement([0x56; 32], 1, 3, b"announcement-g1"),
        ] {
            assert!(matches!(
                verify_announcement_before_reset(&other, &proof(&leaves, 1), 2, &root),
                Err(Error::InclusionProofMismatch)
            ));
        }
        // An identity leaf's proof does not prove an announcement.
        assert!(matches!(
            verify_announcement_before_reset(&ann, &proof(&leaves, 0), 2, &root),
            Err(Error::InclusionProofMismatch)
        ));
    }

    #[test]
    fn the_leaf_is_pinned_and_differs_from_identity_leaves() {
        let ann = AnnouncementLeaf::for_announcement(USER, 1, 3, b"x");
        let bytes = ann.canonical_bytes();
        assert_eq!(bytes.len(), ANNOUNCEMENT_LEAF_LEN);
        assert!(bytes.starts_with(b"lakko/transparency/reset-seals-leaf/v1\0"));
        assert_eq!(&bytes[39..71], &USER);
        assert_eq!(&bytes[71..79], &1u64.to_be_bytes());
        assert_eq!(&bytes[79..87], &3u64.to_be_bytes());
        assert_eq!(&bytes[87..], &hash::blake3(b"x"));
        // An in-memory log appends identity entries only; its leaves are
        // never announcement-shaped.
        let mut log = InMemoryLog::new(SigningKey::from_seed(&[2; 32]));
        log.append(LogEntry {
            username_hash: USER,
            identity_pubkey: vec![0; 32],
            version: 1,
        });
        let entry = log.entry(0).unwrap().canonical_bytes();
        assert!(!entry.starts_with(ANNOUNCEMENT_LEAF_DOMAIN));
    }
}
