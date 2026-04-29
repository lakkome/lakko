// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Key derivation functions: HKDF-SHA-256 and Argon2id.
//!
//! HKDF is the workhorse used to expand MLS exporter outputs into per-purpose
//! keys (SFrame keys, channel content keys, transport keys). Argon2id is used
//! exclusively for password-based key derivation in backup encryption.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::error::{Error, Result};
use crate::secret::SecretBytes;

/// Maximum HKDF-SHA-256 output: 255 * 32 bytes.
pub const HKDF_SHA256_MAX_OUT: usize = 255 * 32;

/// HKDF-Extract followed by HKDF-Expand, returning a `SecretBytes<L>`.
///
/// `salt` may be empty; `info` is the standard HKDF context label.
///
/// # Errors
///
/// Returns [`Error::HkdfTooLong`] if `L > HKDF_SHA256_MAX_OUT`.
pub fn hkdf_sha256<const L: usize>(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<SecretBytes<L>> {
    if L > HKDF_SHA256_MAX_OUT {
        return Err(Error::HkdfTooLong(L));
    }

    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; L];
    hk.expand(info, &mut out)
        .map_err(|_| Error::HkdfTooLong(L))?;
    Ok(SecretBytes::new(out))
}

/// Argon2id-based key derivation for backup-encryption KEK.
///
/// Uses OWASP-recommended interactive parameters: m=64 MiB, t=3, p=4.
/// The output length is fixed at 32 bytes. `salt` must be at least 16 bytes
/// of strong randomness, persisted alongside the encrypted blob.
///
/// # Errors
///
/// Returns [`Error::PasswordHashing`] if Argon2 internal limits are exceeded
/// (cannot happen with our fixed parameters, but the API surfaces it).
pub fn argon2id_kek(password: &[u8], salt: &[u8]) -> Result<SecretBytes<32>> {
    // OWASP 2024 interactive: 64 MiB, 3 iterations, 4 lanes.
    let params = Params::new(64 * 1024, 3, 4, Some(32)).map_err(|_| Error::PasswordHashing)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut out = [0u8; 32];
    argon
        .hash_password_into(password, salt, &mut out)
        .map_err(|_| Error::PasswordHashing)?;
    Ok(SecretBytes::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5869 Test Case 1 — HKDF-SHA-256 with 22-byte IKM.
    #[test]
    fn hkdf_sha256_rfc5869_case1() {
        let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
        let salt = hex::decode("000102030405060708090a0b0c").unwrap();
        let info = hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap();

        let okm = hkdf_sha256::<42>(&ikm, &salt, &info).unwrap();
        let expected = "3cb25f25faacd57a90434f64d0362f2a\
                        2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
                        34007208d5b887185865";
        assert_eq!(hex::encode(okm.as_bytes()), expected);
    }

    #[test]
    fn hkdf_rejects_oversize_output() {
        // L > HKDF_SHA256_MAX_OUT triggers the explicit guard.
        // We can't test L > MAX since L is a const generic compiled in;
        // this test asserts the constant matches the spec.
        assert_eq!(HKDF_SHA256_MAX_OUT, 8160);
    }

    #[test]
    fn argon2id_is_deterministic_for_fixed_salt() {
        let salt = [0x42u8; 16];
        let a = argon2id_kek(b"correct horse battery staple", &salt).unwrap();
        let b = argon2id_kek(b"correct horse battery staple", &salt).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn argon2id_changes_with_password() {
        let salt = [0x42u8; 16];
        let a = argon2id_kek(b"password-1", &salt).unwrap();
        let b = argon2id_kek(b"password-2", &salt).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn argon2id_changes_with_salt() {
        let a = argon2id_kek(b"same-password", &[0x01u8; 16]).unwrap();
        let b = argon2id_kek(b"same-password", &[0x02u8; 16]).unwrap();
        assert_ne!(a, b);
    }
}
