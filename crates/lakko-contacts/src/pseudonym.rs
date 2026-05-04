// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Pseudonym types per ADR-0005 §13.7.
//!
//! Lakko routes everything through pseudonyms — 32-byte identifiers
//! the server uses for delivery without ever seeing real identities.
//! Three lifecycle classes, each derived deterministically from a
//! different keying source so leaks of one class don't compromise
//! the others.
//!
//! | Class                  | Derivation                                            | Lifecycle                                                   |
//! |------------------------|-------------------------------------------------------|-------------------------------------------------------------|
//! | [`IdentityPseudonym`]  | `BLAKE3("lakko-pseudonym/v1/identity", identity_pk)`  | Permanent; rotates only via KT-log supersession proof.      |
//! | [`StrangerInbox`]      | `BLAKE3-keyed("lakko-pseudonym/v1/stranger", k_pub)`  | Long-lived; manual rotation tears down pending invites.     |
//! | [`PerRelationship`]    | `BLAKE3-keyed("lakko-pseudonym/v1/rel", k_seed, ctx)` | Per-contact; rotated on demand without breaking the chat.   |
//!
//! All three are "pseudonyms" in the same observable sense to the
//! server — opaque 32 bytes. The class only matters to the client
//! (different inputs, different rotation rules).

use lakko_crypto::hash;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::VerifyingKey;

use crate::error::{ContactError, Result};

/// Length of every pseudonym variant: 32 raw bytes, matches what
/// `services/ds` and `services/push-gw` accept on the wire.
pub const PSEUDONYM_LEN: usize = 32;

/// Domain separator for [`IdentityPseudonym::from_identity`].
const IDENTITY_INFO: &[u8] = b"lakko-pseudonym/v1/identity";
/// Keyed BLAKE3 context for [`StrangerInbox::derive`].
const STRANGER_INFO: &[u8] = b"lakko-pseudonym/v1/stranger";
/// Keyed BLAKE3 context for [`PerRelationship::derive`].
const RELATIONSHIP_INFO: &[u8] = b"lakko-pseudonym/v1/relationship";

/// Permanent pseudonym tied to the long-term identity public key.
///
/// `BLAKE3(domain_separator || identity_pk_bytes)` is deterministic
/// and one-way: anyone with the public key can compute it (used by
/// transparency log + invite resolution flows), but no information
/// about the private key leaks. Rotation = KT-log supersession proof
/// (handled at the MLS / KT layer, not here).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct IdentityPseudonym([u8; PSEUDONYM_LEN]);

impl IdentityPseudonym {
    /// Derive from the long-term identity public key.
    #[must_use]
    pub fn from_identity(identity_pk: &VerifyingKey) -> Self {
        let mut input = Vec::with_capacity(IDENTITY_INFO.len() + 32);
        input.extend_from_slice(IDENTITY_INFO);
        input.extend_from_slice(&identity_pk.to_bytes());
        Self(hash::blake3(&input))
    }

    /// Wrap raw bytes (e.g. parsed off a wire format).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PSEUDONYM_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PSEUDONYM_LEN] {
        &self.0
    }
}

/// Long-lived inbox for cold contact-requests from strangers. The
/// owner publishes it (out-of-band, e.g. on a profile page or QR
/// code) so people who don't yet have a per-relationship pseudonym
/// can reach them.
///
/// Derived as a keyed BLAKE3 over a 32-byte secret only the owner
/// knows. Rotation = pick a fresh secret (and re-publish), which
/// silently tears down every in-flight stranger request — that's
/// the desired anti-stalking property.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct StrangerInbox([u8; PSEUDONYM_LEN]);

impl StrangerInbox {
    /// Derive from the owner's stranger-inbox secret. The secret
    /// must be 32 bytes of fresh entropy; clients persist it locally
    /// and rotate by generating a new one.
    #[must_use]
    pub fn derive(secret: &SecretBytes<32>) -> Self {
        Self(hash::blake3_keyed(secret.as_bytes(), STRANGER_INFO))
    }

    /// Wrap raw bytes (e.g. read off a wire format).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PSEUDONYM_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PSEUDONYM_LEN] {
        &self.0
    }
}

/// Routing pseudonym scoped to a single contact relationship.
///
/// Derived from a per-relationship 32-byte seed plus the **direction**
/// — Alice-talks-to-Bob and Bob-talks-to-Alice get distinct
/// pseudonyms even with the same seed, so traffic-analysis on the
/// server side cannot pair them up.
///
/// The seed itself is established when the contact request is
/// accepted (via the sealed envelope); both sides retain it locally
/// and never publish it.
///
/// Rotation: pick a fresh seed and re-derive — the chat stays alive
/// because both sides update simultaneously through an ack message
/// inside the existing MLS group.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PerRelationship([u8; PSEUDONYM_LEN]);

/// Direction of a per-relationship pseudonym. Distinct values
/// guarantee distinct pseudonyms for the same `seed`, breaking
/// trivial server-side pair correlation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Self-to-Peer: addresses a message FROM us TO the contact.
    Outbound,
    /// Peer-to-Self: addresses a message FROM the contact TO us.
    Inbound,
}

impl Direction {
    const fn label(self) -> &'static [u8] {
        match self {
            Direction::Outbound => b"out",
            Direction::Inbound => b"in",
        }
    }
}

impl PerRelationship {
    /// Derive from the shared 32-byte relationship seed. The
    /// `direction` ensures Alice-side and Bob-side derivations of
    /// the same seed land on different pseudonyms.
    #[must_use]
    pub fn derive(seed: &SecretBytes<32>, direction: Direction) -> Self {
        let mut input = Vec::with_capacity(RELATIONSHIP_INFO.len() + 1 + direction.label().len());
        input.extend_from_slice(RELATIONSHIP_INFO);
        input.push(b'/');
        input.extend_from_slice(direction.label());
        Self(hash::blake3_keyed(seed.as_bytes(), &input))
    }

    /// Wrap raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PSEUDONYM_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PSEUDONYM_LEN] {
        &self.0
    }
}

// ============================================================================
// Wire-format helpers (used by `request.rs`)
// ============================================================================

/// Decode a 32-byte pseudonym from a slice.
pub(crate) fn decode_pseudonym(slice: &[u8]) -> Result<[u8; PSEUDONYM_LEN]> {
    if slice.len() != PSEUDONYM_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "pseudonym must be {PSEUDONYM_LEN} bytes (got {})",
            slice.len(),
        )));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    out.copy_from_slice(slice);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn identity_pseudonym_is_deterministic() {
        let sk = SigningKey::from_seed(&[7u8; 32]);
        let p1 = IdentityPseudonym::from_identity(&sk.verifying_key());
        let p2 = IdentityPseudonym::from_identity(&sk.verifying_key());
        assert_eq!(p1, p2);
    }

    #[test]
    fn identity_pseudonym_differs_per_key() {
        let alice = SigningKey::from_seed(&[1u8; 32]);
        let bob = SigningKey::from_seed(&[2u8; 32]);
        assert_ne!(
            IdentityPseudonym::from_identity(&alice.verifying_key()),
            IdentityPseudonym::from_identity(&bob.verifying_key()),
        );
    }

    #[test]
    fn stranger_inbox_is_deterministic_and_secret_dependent() {
        let s1 = SecretBytes::<32>::new([0xAA; 32]);
        let s2 = SecretBytes::<32>::new([0xBB; 32]);
        assert_eq!(StrangerInbox::derive(&s1), StrangerInbox::derive(&s1));
        assert_ne!(StrangerInbox::derive(&s1), StrangerInbox::derive(&s2));
    }

    #[test]
    fn stranger_inbox_class_separates_from_other_classes() {
        let s = SecretBytes::<32>::new([0xCD; 32]);
        let stranger = StrangerInbox::derive(&s);

        // Same secret used for a relationship pseudonym must not
        // produce the same bytes — domain separation is exactly
        // what BLAKE3-keyed buys us.
        let rel = PerRelationship::derive(&s, Direction::Outbound);
        assert_ne!(stranger.as_bytes(), rel.as_bytes());
    }

    #[test]
    fn per_relationship_directions_diverge() {
        let seed = SecretBytes::<32>::new([0x55; 32]);
        let outbound = PerRelationship::derive(&seed, Direction::Outbound);
        let inbound = PerRelationship::derive(&seed, Direction::Inbound);
        assert_ne!(
            outbound, inbound,
            "Outbound and Inbound must yield distinct pseudonyms (anti-pairing)",
        );
    }

    #[test]
    fn per_relationship_seeds_diverge() {
        let s1 = SecretBytes::<32>::new([1; 32]);
        let s2 = SecretBytes::<32>::new([2; 32]);
        assert_ne!(
            PerRelationship::derive(&s1, Direction::Outbound),
            PerRelationship::derive(&s2, Direction::Outbound),
        );
    }

    #[test]
    fn from_bytes_roundtrips() {
        let bytes = [0xABu8; PSEUDONYM_LEN];
        assert_eq!(IdentityPseudonym::from_bytes(bytes).as_bytes(), &bytes);
        assert_eq!(StrangerInbox::from_bytes(bytes).as_bytes(), &bytes);
        assert_eq!(PerRelationship::from_bytes(bytes).as_bytes(), &bytes);
    }

    #[test]
    fn decode_pseudonym_rejects_wrong_length() {
        assert!(decode_pseudonym(&[]).is_err());
        assert!(decode_pseudonym(&[0u8; 31]).is_err());
        assert!(decode_pseudonym(&[0u8; 33]).is_err());
        assert!(decode_pseudonym(&[0u8; 32]).is_ok());
    }
}
