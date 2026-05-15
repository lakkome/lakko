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

/// BLAKE3 keyed XOF — extract an arbitrary number of bytes from the BLAKE3
/// stream keyed by `key`. Used where a single fixed 32-byte digest isn't
/// enough (e.g. the 60-byte safety-number derivation in
/// `lakko-contacts::safety`).
///
/// The `derive_key` ctx is the user-facing way to bind a domain to BLAKE3's
/// keyed mode (RFC-style "context string"); a 32-byte key from
/// `derive_key(ctx, "")` then feeds `keyed_hash`'s XOF mode.
#[must_use]
pub fn blake3_keyed_xof(key: &[u8; 32], input: &[u8], out_len: usize) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(input);
    let mut xof = hasher.finalize_xof();
    let mut buf = vec![0u8; out_len];
    xof.fill(&mut buf);
    buf
}

/// `blake3::derive_key(ctx, "")` — yield a 32-byte domain-bound key
/// suitable for [`blake3_keyed`] / [`blake3_keyed_xof`].
///
/// # Panics
///
/// Panics if `domain` is not valid UTF-8. Callers pass an `&'static str`
/// from a `const` declaration in their own module, so this is a
/// compile-time check in practice; the function is `#[must_use]` to
/// surface bugs where the derived key is ignored.
#[must_use]
pub fn blake3_derive_key(domain: &str) -> [u8; 32] {
    blake3::derive_key(domain, b"")
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

/// HMAC-SHA-256 of `message` under `key` — RFC 2104 with SHA-256 as the
/// underlying hash. Use for protocol interop where the spec mandates HMAC
/// over a non-BLAKE3 hash; prefer [`blake3_keyed()`] for new internal
/// designs.
///
/// `key` is accepted as `&[u8]` rather than a fixed 32-byte array because
/// HMAC's spec admits keys of arbitrary length (RFC 2104 §2 — keys longer
/// than the block size are pre-hashed; shorter keys are zero-padded). The
/// caller is responsible for providing a key with at least the security
/// margin the surrounding protocol requires.
///
/// `message.len()` is unbounded — HMAC is streaming-safe.
///
/// # Panics
///
/// Never. The underlying `Hmac::<Sha256>::new_from_slice` is documented
/// to accept arbitrary key lengths (its only error variant exists for
/// fixed-key-size MACs, which HMAC is not). The unwrap is therefore
/// infallible by construction; we surface it via `expect` rather than
/// `unwrap` so a future `hmac` API change that adds real failure modes
/// shows up as a clear error message rather than a generic panic.
#[must_use]
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; DIGEST_LEN] {
    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .expect("HMAC-SHA-256 accepts arbitrary key lengths per RFC 2104");
    mac.update(message);
    mac.finalize().into_bytes().into()
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
    fn hmac_sha256_known_vector() {
        // RFC 4231 test case 1.
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        );
    }

    #[test]
    fn hmac_sha256_different_key_differs() {
        let m = b"hello";
        let a = hmac_sha256(&[0x01u8; 32], m);
        let b = hmac_sha256(&[0x02u8; 32], m);
        assert_ne!(a, b);
    }

    #[test]
    fn hmac_sha256_different_message_differs() {
        let k = [0x42u8; 32];
        let a = hmac_sha256(&k, b"alpha");
        let b = hmac_sha256(&k, b"beta");
        assert_ne!(a, b);
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
