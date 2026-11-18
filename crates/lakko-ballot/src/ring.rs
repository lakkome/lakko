// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The electorate of an anonymous poll and its versions (ADR-0090 section 3.4).
//!
//! Version 0 is the distinct account keys of the roster at the poll's epoch,
//! sorted bytewise. Each later version appends the accounts a merged commit
//! added that were not yet in the ring, sorted bytewise among themselves.
//! Versions are therefore prefixes of one stored list, and version `k` is
//! the first `L_k` keys. Every member applies the same commits in the same
//! order, so every member derives the same versions without any message.
//!
//! `ring_hash_k = BLAKE3-derive-key("lakko/poll/ring/v1", first L_k keys)`.
//!
//! The ring stores keys as bytes. A ballot's proof is over an anonymity set
//! of at most [`crate::ballot::ANONYMITY_SET`] of them, and only those are
//! decoded as points, by [`Ring::point`], when a ballot is cast or verified;
//! a key that is not a valid point makes every set that contains it
//! unusable (fail-closed), and the prover's draw skips it.

use std::collections::HashMap;

use curve25519_dalek::edwards::EdwardsPoint;

use crate::error::{BallotError, Result};
use crate::point::{decode_nonidentity_point, POINT_LEN};

/// The largest ring: 262,144 accounts. Chosen, not measured. Prover and
/// verifier cost no longer depend on it (a proof covers at most 256 keys);
/// what it bounds is what each member stores per poll (32 bytes a key,
/// 8 MiB at the cap) and the open-items hand-over of ADR-0090 section 3.9,
/// which at the cap spans about nine 1 MiB DS rows.
pub const MAX_RING_LEN: usize = 1 << 18;

/// The largest number of versions.
pub const MAX_VERSIONS: usize = u16::MAX as usize;

/// Domain of the ring hash (ADR-0090 section 3.4).
pub const RING_HASH_DOMAIN: &str = "lakko/poll/ring/v1";

/// `BLAKE3-derive-key("lakko/poll/ring/v1", keys)` over the concatenated keys.
#[must_use]
pub fn ring_hash(keys: &[[u8; POINT_LEN]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(RING_HASH_DOMAIN);
    for key in keys {
        hasher.update(key);
    }
    *hasher.finalize().as_bytes()
}

/// A poll's ring and its versions.
#[derive(Clone)]
pub struct Ring {
    keys: Vec<[u8; POINT_LEN]>,
    lengths: Vec<u32>,
    hashes: Vec<[u8; 32]>,
    positions: HashMap<[u8; POINT_LEN], u32>,
    hasher: blake3::Hasher,
}

impl core::fmt::Debug for Ring {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ring")
            .field("len", &self.keys.len())
            .field("lengths", &self.lengths)
            .finish_non_exhaustive()
    }
}

fn strictly_ascending(keys: &[[u8; POINT_LEN]]) -> bool {
    keys.windows(2).all(|w| w[0] < w[1])
}

impl Ring {
    /// Version 0: `keys` must be non-empty, strictly ascending (so distinct)
    /// and at most [`MAX_RING_LEN`] long.
    pub fn new(keys: &[[u8; POINT_LEN]]) -> Result<Self> {
        if keys.is_empty() || keys.len() > MAX_RING_LEN || !strictly_ascending(keys) {
            return Err(BallotError::InvalidRing);
        }
        let mut ring = Self {
            keys: Vec::with_capacity(keys.len()),
            lengths: Vec::new(),
            hashes: Vec::new(),
            positions: HashMap::with_capacity(keys.len()),
            hasher: blake3::Hasher::new_derive_key(RING_HASH_DOMAIN),
        };
        ring.push_batch(keys);
        Ok(ring)
    }

    fn push_batch(&mut self, batch: &[[u8; POINT_LEN]]) {
        for key in batch {
            // At most MAX_RING_LEN keys, which fits a u32.
            let position = u32::try_from(self.keys.len()).unwrap_or(u32::MAX);
            self.keys.push(*key);
            self.positions.insert(*key, position);
            self.hasher.update(key);
        }
        self.lengths
            .push(u32::try_from(self.keys.len()).unwrap_or(u32::MAX));
        self.hashes.push(*self.hasher.finalize().as_bytes());
    }

    /// Apply a merged commit: append the keys of `added` not yet in the ring,
    /// sorted bytewise and de-duplicated. Returns the new version's index, or
    /// `None` when the commit added nobody new (no version is made).
    ///
    /// The ring is left unchanged on error: past [`MAX_RING_LEN`] or
    /// [`MAX_VERSIONS`] it stops growing, the same way on every member.
    pub fn extend(&mut self, added: &[[u8; POINT_LEN]]) -> Result<Option<usize>> {
        let mut batch: Vec<[u8; POINT_LEN]> = added
            .iter()
            .filter(|k| !self.positions.contains_key(*k))
            .copied()
            .collect();
        batch.sort_unstable();
        batch.dedup();
        if batch.is_empty() {
            return Ok(None);
        }
        if self.keys.len() + batch.len() > MAX_RING_LEN || self.lengths.len() >= MAX_VERSIONS {
            return Err(BallotError::RingTooLarge);
        }
        self.push_batch(&batch);
        Ok(Some(self.lengths.len() - 1))
    }

    /// Every key, in ring order.
    #[must_use]
    pub fn keys(&self) -> &[[u8; POINT_LEN]] {
        &self.keys
    }

    /// The number of versions (at least 1).
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.lengths.len()
    }

    /// `L_k`, the number of keys in version `k`.
    #[must_use]
    pub fn version_len(&self, version: usize) -> Option<usize> {
        self.lengths.get(version).map(|&l| l as usize)
    }

    /// `ring_hash_k`.
    #[must_use]
    pub fn version_hash(&self, version: usize) -> Option<[u8; 32]> {
        self.hashes.get(version).copied()
    }

    /// The version whose hash is `hash`.
    #[must_use]
    pub fn find_version(&self, hash: &[u8; 32]) -> Option<usize> {
        self.hashes.iter().position(|h| h == hash)
    }

    /// Whether `key` is in the ring (any version).
    #[must_use]
    pub fn contains(&self, key: &[u8; POINT_LEN]) -> bool {
        self.positions.contains_key(key)
    }

    /// The position of `key` in the ring.
    #[must_use]
    pub fn position(&self, key: &[u8; POINT_LEN]) -> Option<usize> {
        self.positions.get(key).map(|&p| p as usize)
    }

    /// The key at `index` decoded as a point: canonical, torsion-free, not
    /// the identity. [`BallotError::RingVersionUnusable`] for a key that is
    /// not, or for an index past the ring.
    pub(crate) fn point(&self, index: usize) -> Result<EdwardsPoint> {
        let key = self
            .keys
            .get(index)
            .ok_or(BallotError::RingVersionUnusable)?;
        decode_nonidentity_point(key).map_err(|_| BallotError::RingVersionUnusable)
    }

    /// Encode as `ring_count(u32 BE) || ring_count x key(32) ||
    /// version_count(u16 BE) || version_count x length(u32 BE)`, the layout the
    /// open-items hand-over of ADR-0090 section 3.9 carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.keys.len() * POINT_LEN + self.lengths.len() * 4);
        out.extend_from_slice(
            &u32::try_from(self.keys.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for key in &self.keys {
            out.extend_from_slice(key);
        }
        out.extend_from_slice(
            &u16::try_from(self.lengths.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        for len in &self.lengths {
            out.extend_from_slice(&len.to_be_bytes());
        }
        out
    }

    /// Decode [`Ring::encode`]'s layout, strictly: exact length, lengths
    /// strictly increasing and ending at the key count, version 0 and every
    /// extension batch strictly ascending, no key twice. Re-encoding a decoded
    /// ring gives the same bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (count, rest) = split_u32(bytes)?;
        let count = count as usize;
        if count == 0 || count > MAX_RING_LEN {
            return Err(BallotError::InvalidRing);
        }
        let key_bytes = count.checked_mul(POINT_LEN).ok_or(BallotError::Malformed)?;
        if rest.len() < key_bytes {
            return Err(BallotError::Malformed);
        }
        let (key_area, rest) = rest.split_at(key_bytes);
        let keys: Vec<[u8; POINT_LEN]> = key_area
            .chunks_exact(POINT_LEN)
            .map(|c| {
                let mut k = [0u8; POINT_LEN];
                k.copy_from_slice(c);
                k
            })
            .collect();
        let (&hi, rest) = rest.split_first().ok_or(BallotError::Malformed)?;
        let (&lo, rest) = rest.split_first().ok_or(BallotError::Malformed)?;
        let versions = usize::from(u16::from_be_bytes([hi, lo]));
        if versions == 0 || rest.len() != versions * 4 {
            return Err(BallotError::Malformed);
        }
        let lengths: Vec<usize> = rest
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize)
            .collect();
        if lengths.windows(2).any(|w| w[0] >= w[1])
            || lengths.last() != Some(&count)
            || lengths[0] == 0
        {
            return Err(BallotError::InvalidRing);
        }
        let mut ring = Self::new(&keys[..lengths[0]])?;
        for w in lengths.windows(2) {
            let batch = &keys[w[0]..w[1]];
            if !strictly_ascending(batch) || batch.iter().any(|k| ring.contains(k)) {
                return Err(BallotError::InvalidRing);
            }
            ring.push_batch(batch);
        }
        Ok(ring)
    }
}

fn split_u32(bytes: &[u8]) -> Result<(u32, &[u8])> {
    if bytes.len() < 4 {
        return Err(BallotError::Malformed);
    }
    let (head, rest) = bytes.split_at(4);
    Ok((
        u32::from_be_bytes([head[0], head[1], head[2], head[3]]),
        rest,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::VoterKey;

    fn key(i: u8) -> [u8; 32] {
        *VoterKey::from_ed25519_seed(&[i; 32]).public_key()
    }

    fn sorted(mut v: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
        v.sort_unstable();
        v
    }

    #[test]
    fn versions_are_prefixes_and_hash_like_the_adr() {
        let v0 = sorted(vec![key(1), key(2), key(3)]);
        let mut ring = Ring::new(&v0).unwrap();
        assert_eq!(ring.version_hash(0).unwrap(), ring_hash(&v0));
        assert_eq!(ring.extend(&[key(2)]).unwrap(), None);
        assert_eq!(ring.extend(&[key(5), key(4), key(5)]).unwrap(), Some(1));
        assert_eq!(ring.version_len(1), Some(5));
        assert_eq!(ring.version_hash(1).unwrap(), ring_hash(ring.keys()));
        assert_eq!(&ring.keys()[3..], sorted(vec![key(4), key(5)]).as_slice());
        let again = Ring::decode(&ring.encode()).unwrap();
        assert_eq!(again.encode(), ring.encode());
        assert_eq!(again.find_version(&ring.version_hash(1).unwrap()), Some(1));
    }

    #[test]
    fn version_zero_must_be_strictly_ascending() {
        let v = sorted(vec![key(1), key(2)]);
        assert!(Ring::new(&[v[1], v[0]]).is_err());
        assert!(Ring::new(&[v[0], v[0]]).is_err());
        assert!(Ring::new(&[]).is_err());
    }

    #[test]
    fn an_invalid_key_is_stored_but_does_not_decode() {
        let v0 = sorted(vec![key(1), key(2)]);
        let mut ring = Ring::new(&v0).unwrap();
        ring.extend(&[[0u8; 32]]).unwrap(); // not a point
        assert!(ring.point(0).is_ok());
        assert_eq!(ring.point(2), Err(BallotError::RingVersionUnusable));
        assert_eq!(ring.point(3), Err(BallotError::RingVersionUnusable));
        assert_eq!(
            Ring::decode(&ring.encode()).unwrap().encode(),
            ring.encode()
        );
    }
}
