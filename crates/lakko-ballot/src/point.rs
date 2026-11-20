// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group elements, scalars and the voter's secret.
//!
//! The proof works in the prime-order subgroup of Edwards25519, the group
//! Ed25519 account keys live in. Every point that arrives from outside is
//! decoded canonically and refused unless it lies in that subgroup, so no
//! small-order component can enter an equation.
//!
//! Generators whose discrete logarithm nobody may know (the commitment
//! generators and the per-poll tag base) come from [`hash_to_point`], a
//! try-and-increment map: BLAKE3 in derive-key mode over the input and a
//! counter gives a candidate y-coordinate, the first candidate that
//! decompresses is multiplied by the cofactor, and the identity is skipped.
//! This is not RFC 9380 hash-to-curve: the output is not uniform over the
//! subgroup. What the proof needs is that no discrete logarithm relation
//! between these points and the base point is known, which a hash output
//! gives. curve25519-dalek 4.1.3 offers no RFC 9380 map for Edwards points
//! (its `nonspec_map_to_curve` is deprecated and documented as not a secure
//! hash), and Ristretto's map cannot be used because Ed25519 keys are
//! Edwards points and the crate has no public conversion between the two.

use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity;
use zeroize::Zeroize;

use crate::error::{BallotError, Result};

/// Length of an encoded point.
pub const POINT_LEN: usize = 32;
/// Length of an encoded scalar.
pub const SCALAR_LEN: usize = 32;

/// The Ed25519 base point, the `G` of the proof.
pub(crate) const G: EdwardsPoint = ED25519_BASEPOINT_POINT;

/// Decode a point: a canonical encoding of a point in the prime-order
/// subgroup. The identity is accepted (it is in the subgroup); callers that
/// must refuse it use [`decode_nonidentity_point`].
pub(crate) fn decode_point(bytes: &[u8; POINT_LEN]) -> Result<EdwardsPoint> {
    let compressed = CompressedEdwardsY(*bytes);
    let point = compressed.decompress().ok_or(BallotError::InvalidPoint)?;
    // curve25519-dalek reduces a y-coordinate at or above p; the re-encode
    // comparison refuses such non-canonical strings, so each point has one
    // encoding and nothing is malleable.
    if point.compress().to_bytes() != *bytes {
        return Err(BallotError::InvalidPoint);
    }
    if !point.is_torsion_free() {
        return Err(BallotError::InvalidPoint);
    }
    Ok(point)
}

/// [`decode_point`], and the identity refused. Used for ring keys and tags.
pub(crate) fn decode_nonidentity_point(bytes: &[u8; POINT_LEN]) -> Result<EdwardsPoint> {
    let point = decode_point(bytes)?;
    if point.is_identity() {
        return Err(BallotError::InvalidPoint);
    }
    Ok(point)
}

/// Decode a canonical scalar (strictly below the group order).
pub(crate) fn decode_scalar(bytes: &[u8; SCALAR_LEN]) -> Result<Scalar> {
    Option::<Scalar>::from(Scalar::from_canonical_bytes(*bytes)).ok_or(BallotError::InvalidScalar)
}

/// Map `(domain, input)` to a point of the prime-order subgroup whose discrete
/// logarithm with respect to any other point is unknown. See the module
/// documentation for the construction and why it is not RFC 9380.
///
/// Deterministic, and variable time: its inputs are public.
pub(crate) fn hash_to_point(domain: &str, input: &[u8]) -> Result<EdwardsPoint> {
    for counter in 0u8..=u8::MAX {
        let mut hasher = blake3::Hasher::new_derive_key(domain);
        hasher.update(input);
        hasher.update(&[counter]);
        let candidate = CompressedEdwardsY(*hasher.finalize().as_bytes());
        if let Some(point) = candidate.decompress() {
            let cleared = point.mul_by_cofactor();
            if !cleared.is_identity() {
                return Ok(cleared);
            }
        }
    }
    Err(BallotError::HashToPoint)
}

/// Domain of the commitment generators.
const COMMIT_GEN_DOMAIN: &str = "lakko/poll/commit-gen/v1";

/// The commitment generators: `H` (the blinding generator) and `count`
/// vector generators `G_0 .. G_{count-1}`.
pub(crate) fn commitment_generators(count: usize) -> Result<(EdwardsPoint, Vec<EdwardsPoint>)> {
    let h = hash_to_point(COMMIT_GEN_DOMAIN, b"H")?;
    let mut gens = Vec::with_capacity(count);
    for index in 0..count {
        let index = u32::try_from(index).map_err(|_| BallotError::Malformed)?;
        let mut input = [0u8; 5];
        input[0] = b'G';
        input[1..].copy_from_slice(&index.to_be_bytes());
        gens.push(hash_to_point(COMMIT_GEN_DOMAIN, &input)?);
    }
    Ok((h, gens))
}

/// The voter's secret: the Ed25519 signing scalar of the key its leaf in the
/// group is bound to (ADR-0092 section 3.6, "The witness"), and the matching
/// public key. Zeroized on drop.
pub struct VoterKey {
    scalar: Scalar,
    public: [u8; POINT_LEN],
}

impl VoterKey {
    /// The voter key of an Ed25519 seed: the scalar is SHA-512 of the seed,
    /// clamped and reduced, exactly as an Ed25519 signer computes it, so the
    /// public key is the Ed25519 verifying key of that seed.
    #[must_use]
    pub fn from_ed25519_seed(seed: &[u8; 32]) -> Self {
        let signing = ed25519_dalek::SigningKey::from_bytes(seed);
        let scalar = signing.to_scalar();
        let public = signing.verifying_key().to_bytes();
        Self { scalar, public }
    }

    /// The public key, the 32-byte Ed25519 verifying key.
    #[must_use]
    pub const fn public_key(&self) -> &[u8; POINT_LEN] {
        &self.public
    }

    pub(crate) const fn scalar(&self) -> &Scalar {
        &self.scalar
    }
}

impl Drop for VoterKey {
    fn drop(&mut self) {
        self.scalar.zeroize();
    }
}

impl core::fmt::Debug for VoterKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VoterKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_to_point_is_deterministic_and_in_the_subgroup() {
        let a = hash_to_point("lakko/test/v1", b"x").unwrap();
        let b = hash_to_point("lakko/test/v1", b"x").unwrap();
        let c = hash_to_point("lakko/test/v1", b"y").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.is_torsion_free());
        assert!(!a.is_identity());
    }

    #[test]
    fn voter_key_public_is_scalar_times_base() {
        let key = VoterKey::from_ed25519_seed(&[7u8; 32]);
        assert_eq!((G * key.scalar()).compress().to_bytes(), *key.public_key());
    }

    #[test]
    fn small_order_and_noncanonical_points_are_refused() {
        // The point of order 2: (0, -1), y = p - 1.
        let mut order_two = [0xffu8; 32];
        order_two[0] = 0xec;
        order_two[31] = 0x7f;
        assert_eq!(decode_point(&order_two), Err(BallotError::InvalidPoint));
        // y = 1 encoded as p + 1 (non-canonical identity).
        let mut noncanonical = [0xffu8; 32];
        noncanonical[0] = 0xee;
        noncanonical[31] = 0x7f;
        assert_eq!(decode_point(&noncanonical), Err(BallotError::InvalidPoint));
        // The canonical identity is accepted by decode_point, refused as a key.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(decode_point(&identity).is_ok());
        assert_eq!(
            decode_nonidentity_point(&identity),
            Err(BallotError::InvalidPoint)
        );
    }
}
