// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Cryptographically secure random number generation.
//!
//! All randomness in `lakko-crypto` (and crates depending on it) MUST come
//! through this module. Direct use of `OsRng` or `thread_rng` is forbidden
//! by review.

use rand_core::{CryptoRng, OsRng, RngCore};

use crate::secret::SecretBytes;

/// Fill `dst` with cryptographically secure random bytes from the OS.
pub fn fill(dst: &mut [u8]) {
    OsRng.fill_bytes(dst);
}

/// Generate a fresh `SecretBytes<N>` filled with OS randomness.
#[must_use]
pub fn secret_bytes<const N: usize>() -> SecretBytes<N> {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    SecretBytes::new(buf)
}

/// Borrow the OS RNG as a `CryptoRng + RngCore` for libraries that take a
/// generic RNG (e.g. `ed25519-dalek::SigningKey::generate`).
#[must_use]
pub const fn rng() -> impl CryptoRng + RngCore {
    OsRng
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_changes_bytes() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        fill(&mut a);
        fill(&mut b);
        assert_ne!(a, [0u8; 32], "RNG produced all zeros");
        assert_ne!(a, b, "two RNG fills produced same output");
    }

    #[test]
    fn secret_bytes_helper_returns_random() {
        let a = secret_bytes::<32>();
        let b = secret_bytes::<32>();
        assert_ne!(a.as_bytes(), &[0u8; 32]);
        assert_ne!(a, b, "two random secrets collided");
    }
}
