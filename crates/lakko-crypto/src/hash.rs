// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Hashing primitives.
//!
//! BLAKE3 is the default for general-purpose hashing in Lakko (identity
//! fingerprints, content addressing, transcript hashing). SHA-256 is exposed
//! for interop with protocols that mandate it (TLS, X.509).

/// Length of a BLAKE3 / SHA-256 digest in bytes.
pub const DIGEST_LEN: usize = 32;

/// BLAKE3 of the given input.
#[must_use]
pub fn blake3(input: &[u8]) -> [u8; DIGEST_LEN] {
    *blake3::hash(input).as_bytes()
}

/// BLAKE3 keyed hash — used for HMAC-style integrity tags where a faster,
/// simpler primitive is acceptable.
#[must_use]
pub fn blake3_keyed(key: &[u8; 32], input: &[u8]) -> [u8; DIGEST_LEN] {
    *blake3::keyed_hash(key, input).as_bytes()
}

/// SHA-256 of the given input. Use only for protocol interop (X.509, TLS).
/// Prefer [`blake3()`] for new internal designs.
#[must_use]
pub fn sha256(input: &[u8]) -> [u8; DIGEST_LEN] {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_known_vector() {
        // RFC-9106-style empty-input test vector for BLAKE3.
        let digest = blake3(b"");
        assert_eq!(
            hex::encode(digest),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        );
    }

    #[test]
    fn sha256_known_vector() {
        let digest = sha256(b"abc");
        assert_eq!(
            hex::encode(digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn blake3_keyed_differs_from_unkeyed() {
        let key = [0x42u8; 32];
        let unkeyed = blake3(b"hello");
        let keyed = blake3_keyed(&key, b"hello");
        assert_ne!(unkeyed, keyed);
    }

    proptest::proptest! {
        #[test]
        fn blake3_is_deterministic(input: Vec<u8>) {
            let a = blake3(&input);
            let b = blake3(&input);
            proptest::prop_assert_eq!(a, b);
        }

        #[test]
        fn sha256_is_deterministic(input: Vec<u8>) {
            let a = sha256(&input);
            let b = sha256(&input);
            proptest::prop_assert_eq!(a, b);
        }
    }
}
