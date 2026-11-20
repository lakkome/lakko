// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The Fiat-Shamir transcript.
//!
//! A running BLAKE3 hash, keyed in derive-key mode by a crate-wide context and
//! started with a per-protocol label, into which every public input is
//! appended with a label and an explicit length. The challenge is 64 bytes of
//! XOF output reduced modulo the group order. Labels and lengths make the
//! encoding injective: two different sequences of appends cannot hash the same
//! byte stream.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

/// BLAKE3 derive-key context of every transcript in this crate.
const TRANSCRIPT_CONTEXT: &str = "lakko/anon-cred/v1 fiat-shamir";

/// A Fiat-Shamir transcript.
#[derive(Clone)]
pub(crate) struct Transcript {
    hasher: blake3::Hasher,
}

impl Transcript {
    /// A transcript for one protocol. `protocol` names the proof kind and is
    /// the first labelled append, so a proof made for one kind is a proof of
    /// nothing in another.
    pub(crate) fn new(protocol: &[u8]) -> Self {
        let mut transcript = Self {
            hasher: blake3::Hasher::new_derive_key(TRANSCRIPT_CONTEXT),
        };
        transcript.append(b"protocol", protocol);
        transcript
    }

    /// Append `label_len (u16 BE) || label || len (u64 BE) || bytes`.
    pub(crate) fn append(&mut self, label: &[u8], bytes: &[u8]) {
        let label_len = u16::try_from(label.len()).unwrap_or(u16::MAX);
        self.hasher.update(&label_len.to_be_bytes());
        self.hasher.update(label);
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.hasher.update(&len.to_be_bytes());
        self.hasher.update(bytes);
    }

    /// Append a `u64` as 8 big-endian bytes.
    pub(crate) fn append_u64(&mut self, label: &[u8], value: u64) {
        self.append(label, &value.to_be_bytes());
    }

    /// Append a point in its canonical compressed form.
    pub(crate) fn append_point(&mut self, label: &[u8], point: &RistrettoPoint) {
        self.append(label, point.compress().as_bytes());
    }

    /// The challenge: 64 bytes of XOF output over the transcript so far
    /// (after a final `challenge` label), reduced modulo the group order.
    pub(crate) fn challenge(&self) -> Scalar {
        let mut hasher = self.hasher.clone();
        let label: &[u8] = b"challenge";
        let label_len = u16::try_from(label.len()).unwrap_or(u16::MAX);
        hasher.update(&label_len.to_be_bytes());
        hasher.update(label);
        let mut wide = [0u8; 64];
        hasher.finalize_xof().fill(&mut wide);
        Scalar::from_bytes_mod_order_wide(&wide)
    }

    /// A 32-byte digest of the transcript so far, used only to hedge proof
    /// nonces (never as a challenge).
    pub(crate) fn state_digest(&self) -> [u8; 32] {
        *self.hasher.clone().finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_are_unambiguous() {
        let mut a = Transcript::new(b"p");
        a.append(b"ab", b"c");
        let mut b = Transcript::new(b"p");
        b.append(b"a", b"bc");
        assert_ne!(a.challenge(), b.challenge());
    }

    #[test]
    fn protocol_label_separates() {
        assert_ne!(
            Transcript::new(b"one").challenge(),
            Transcript::new(b"two").challenge()
        );
    }

    #[test]
    fn challenge_is_deterministic() {
        let mut a = Transcript::new(b"p");
        a.append_u64(b"n", 7);
        let b = a.clone();
        assert_eq!(a.challenge(), b.challenge());
    }
}
