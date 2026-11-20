// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The Fiat-Shamir transcript.
//!
//! BLAKE3 in derive-key mode under a domain string, fed a sequence of
//! `(label, data)` pairs, each framed as `len(label) u64 BE || label ||
//! len(data) u64 BE || data`, so no two different sequences hash the same
//! bytes. A challenge is 64 bytes of the XOF reduced modulo the group order
//! (`Scalar::from_bytes_mod_order_wide`), whose bias is about 2^-259.

use curve25519_dalek::edwards::EdwardsPoint;
use curve25519_dalek::scalar::Scalar;

pub(crate) struct Transcript {
    hasher: blake3::Hasher,
}

impl Transcript {
    pub(crate) fn new(domain: &str) -> Self {
        Self {
            hasher: blake3::Hasher::new_derive_key(domain),
        }
    }

    pub(crate) fn append(&mut self, label: &[u8], data: &[u8]) {
        self.hasher.update(&(label.len() as u64).to_be_bytes());
        self.hasher.update(label);
        self.hasher.update(&(data.len() as u64).to_be_bytes());
        self.hasher.update(data);
    }

    pub(crate) fn append_point(&mut self, label: &[u8], point: &EdwardsPoint) {
        self.append(label, point.compress().as_bytes());
    }

    pub(crate) fn append_u64(&mut self, label: &[u8], value: u64) {
        self.append(label, &value.to_be_bytes());
    }

    /// Finish the transcript with a challenge label and squeeze one scalar.
    pub(crate) fn challenge(mut self, label: &[u8]) -> Scalar {
        self.append(b"challenge", label);
        let mut wide = [0u8; 64];
        self.hasher.finalize_xof().fill(&mut wide);
        Scalar::from_bytes_mod_order_wide(&wide)
    }
}
