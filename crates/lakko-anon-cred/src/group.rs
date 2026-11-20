// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ristretto255 helpers: the fixed generators, hash-to-group, canonical
//! decoding and fresh random scalars.
//!
//! Every generator is the output of hash-to-group on a fixed label, so nobody
//! (the implementer included) knows a discrete-log relation between any two of
//! them. The scheme's soundness needs exactly that for `G` and `H`.

use std::sync::OnceLock;

use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::Identity;
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use crate::error::{AnonCredError, Result};

/// BLAKE3 derive-key context of every hash-to-group in this crate. The label
/// passed beside it separates the uses.
const HASH_TO_GROUP_CONTEXT: &str = "lakko/anon-cred/v1 hash-to-group";

/// Hash `label || input` to a Ristretto255 point.
///
/// `BLAKE3-derive-key(HASH_TO_GROUP_CONTEXT)` in XOF mode over
/// `label_len (u16 BE) || label || input`, 64 output bytes, mapped with
/// `RistrettoPoint::from_uniform_bytes` (two Elligator maps added, the
/// construction RFC 9496 section 4.3.4 specifies for hash-to-group). The
/// length prefix keeps one (label, input) pair from colliding with another.
pub(crate) fn hash_to_group(label: &[u8], input: &[u8]) -> RistrettoPoint {
    let mut hasher = blake3::Hasher::new_derive_key(HASH_TO_GROUP_CONTEXT);
    let label_len = u16::try_from(label.len()).unwrap_or(u16::MAX);
    hasher.update(&label_len.to_be_bytes());
    hasher.update(label);
    hasher.update(input);
    let mut wide = [0u8; 64];
    hasher.finalize_xof().fill(&mut wide);
    RistrettoPoint::from_uniform_bytes(&wide)
}

/// `G`: the base of the account commitment `C = s * G`, of the MAC base point
/// `U = b * G` at issuance, of the issuer's `x0` commitment and of the
/// blinding of `U'` in a presentation (CMZ14's `g`).
pub(crate) fn generator_g() -> RistrettoPoint {
    static G: OnceLock<RistrettoPoint> = OnceLock::new();
    *G.get_or_init(|| hash_to_group(b"generator G (G_cred)", &[]))
}

/// `H`: the base of the issuer's per-attribute public keys `X_i = x_i * H`
/// and of every Pedersen blinding term (CMZ14's `h`).
pub(crate) fn generator_h() -> RistrettoPoint {
    static H: OnceLock<RistrettoPoint> = OnceLock::new();
    *H.get_or_init(|| hash_to_group(b"generator H", &[]))
}

/// Decode a canonical compressed Ristretto255 point.
pub(crate) fn decode_point(bytes: &[u8]) -> Result<RistrettoPoint> {
    let compressed =
        CompressedRistretto::from_slice(bytes).map_err(|_| AnonCredError::Malformed)?;
    compressed.decompress().ok_or(AnonCredError::NonCanonical)
}

/// Decode a canonical point and refuse the identity.
pub(crate) fn decode_nonidentity_point(bytes: &[u8]) -> Result<RistrettoPoint> {
    let point = decode_point(bytes)?;
    if point == RistrettoPoint::identity() {
        return Err(AnonCredError::IdentityPoint);
    }
    Ok(point)
}

/// Decode a scalar that must be below the group order (canonical).
pub(crate) fn decode_scalar(bytes: &[u8]) -> Result<Scalar> {
    let array: [u8; 32] = bytes.try_into().map_err(|_| AnonCredError::Malformed)?;
    Option::from(Scalar::from_canonical_bytes(array)).ok_or(AnonCredError::NonCanonical)
}

/// A uniformly random non-zero scalar from the operating system's RNG.
///
/// 64 random bytes reduced modulo the group order (statistical distance from
/// uniform about 2^-260). The zero scalar is redrawn; reaching the redraw
/// bound needs a broken RNG and is reported as unavailable randomness.
pub(crate) fn random_nonzero_scalar() -> Result<Scalar> {
    for _ in 0..8 {
        let mut wide = Zeroizing::new([0u8; 64]);
        OsRng
            .try_fill_bytes(wide.as_mut())
            .map_err(|_| AnonCredError::RandomnessUnavailable)?;
        let scalar = Scalar::from_bytes_mod_order_wide(&wide);
        if scalar != Scalar::ZERO {
            return Ok(scalar);
        }
    }
    Err(AnonCredError::RandomnessUnavailable)
}

/// 32 fresh random bytes from the operating system's RNG.
pub(crate) fn random_bytes() -> Result<Zeroizing<[u8; 32]>> {
    let mut out = Zeroizing::new([0u8; 32]);
    OsRng
        .try_fill_bytes(out.as_mut())
        .map_err(|_| AnonCredError::RandomnessUnavailable)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_distinct_and_not_identity() {
        let g = generator_g();
        let h = generator_h();
        assert_ne!(g, h);
        assert_ne!(g, RistrettoPoint::identity());
        assert_ne!(h, RistrettoPoint::identity());
        assert_ne!(g, curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT);
    }

    #[test]
    fn hash_to_group_separates_label_from_input() {
        assert_ne!(hash_to_group(b"ab", b"c"), hash_to_group(b"a", b"bc"));
        assert_eq!(hash_to_group(b"x", b"y"), hash_to_group(b"x", b"y"));
    }

    #[test]
    fn decoders_refuse_bad_input() {
        assert_eq!(decode_point(&[0u8; 31]), Err(AnonCredError::Malformed));
        assert_eq!(
            decode_nonidentity_point(&[0u8; 32]),
            Err(AnonCredError::IdentityPoint)
        );
        assert_eq!(
            decode_point(&[0xffu8; 32]),
            Err(AnonCredError::NonCanonical)
        );
        assert_eq!(
            decode_scalar(&[0xffu8; 32]),
            Err(AnonCredError::NonCanonical)
        );
        assert_eq!(decode_scalar(&[0u8; 33]), Err(AnonCredError::Malformed));
    }
}
