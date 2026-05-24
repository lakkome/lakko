// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel epoch counter.
//!
//! Per ADR-0010 § "Epoch boundaries — when the seed rotates" — a
//! new [`EpochId`] is emitted on every PublisherGroup commit and on
//! every scheduled or manual rotation. It binds posts to the
//! `content_key_seed` they were encrypted under, and is included in
//! both the HKDF salt (so two epochs sharing a sender don't collide
//! on per-message keys) and the AAD on every post (so a replay
//! across an epoch boundary fails the AEAD open).
//!
//! ## Wire shape
//!
//! [`EPOCH_ID_LEN`] is 4 bytes — a `u32` big-endian. 4 billion
//! rotations covers the channel's entire useful life: at the
//! per-channel ceiling of "rotation every 100k posts OR every 24h",
//! a constantly-saturated channel hits the ceiling in
//! ~292 277 years. We pin a u32 explicitly so an interop fuzzer
//! can't push us into a u64-vs-u32 confusion.

use crate::error::{ChannelError, Result};

/// Big-endian width of an [`EpochId`] on the wire.
pub const EPOCH_ID_LEN: usize = 4;

/// Monotone per-channel epoch counter.
///
/// Wraps a `u32`. Implements `Ord` so a deserialised post can be
/// compared against the publisher's current epoch to detect
/// rollback attempts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpochId(u32);

impl EpochId {
    /// The first epoch, emitted when a channel is created.
    pub const GENESIS: Self = Self(0);

    /// Construct from an explicit `u32`. The caller is responsible
    /// for monotonicity; the type doesn't enforce it.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Raw underlying `u32`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Next epoch. Saturates at `u32::MAX`; in practice we never
    /// approach that ceiling (see crate-level wire-shape note).
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// 4-byte big-endian representation, the form that goes into
    /// HKDF salt + AAD + signature payload.
    #[must_use]
    pub const fn to_be_bytes(self) -> [u8; EPOCH_ID_LEN] {
        self.0.to_be_bytes()
    }

    /// Parse an [`EpochId`] from a 4-byte big-endian slice.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Wire`] if `bytes.len() != EPOCH_ID_LEN`.
    pub fn from_be_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; EPOCH_ID_LEN] = bytes
            .try_into()
            .map_err(|_| ChannelError::Wire("epoch id must be 4 bytes"))?;
        Ok(Self(u32::from_be_bytes(arr)))
    }
}

impl From<u32> for EpochId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_is_zero() {
        assert_eq!(EpochId::GENESIS.as_u32(), 0);
    }

    #[test]
    fn next_is_monotone() {
        let a = EpochId::new(42);
        let b = a.next();
        assert!(b > a);
        assert_eq!(b.as_u32(), 43);
    }

    #[test]
    fn next_saturates() {
        let max = EpochId::new(u32::MAX);
        assert_eq!(max.next(), max);
    }

    #[test]
    fn be_roundtrip() {
        let e = EpochId::new(0xDEAD_BEEF);
        let bytes = e.to_be_bytes();
        assert_eq!(bytes, [0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(EpochId::from_be_bytes(&bytes).unwrap(), e);
    }

    #[test]
    fn parse_rejects_short_buffer() {
        assert!(EpochId::from_be_bytes(&[0, 0, 0]).is_err());
        assert!(EpochId::from_be_bytes(&[0, 0, 0, 0, 0]).is_err());
    }
}
