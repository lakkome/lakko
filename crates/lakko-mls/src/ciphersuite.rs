// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Pinned MLS ciphersuites for Lakko.
//!
//! Per ADR-0001 we support exactly two ciphersuites:
//!
//! - [`DEFAULT`] — `MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519`
//!   (MLS suite ID 3), the fast software path used by default.
//! - [`FIPS`] — `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` (MLS suite ID 5),
//!   the FIPS-friendly path for deployments that require it.
//!
//! The `128`/`256` prefix in the MLS suite name denotes the symmetric
//! security tier per the MLS spec, not the literal key length — both suites
//! provide ≥128-bit security and use 256-bit keys for AEAD/MAC.
//!
//! No other ciphersuites are accepted by Lakko clients.

use openmls::prelude::Ciphersuite;

/// Default Lakko ciphersuite — X25519 + ChaCha20-Poly1305 + SHA-256 + Ed25519.
///
/// MLS suite ID 3. Constant-time pure-software primitives, fast on every
/// platform regardless of hardware AES support.
pub const DEFAULT: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

/// FIPS-friendly Lakko ciphersuite — P-384 + AES-256-GCM + SHA-384 + ECDSA-P384.
///
/// MLS suite ID 5. Used only where compliance requires NIST primitives.
pub const FIPS: Ciphersuite = Ciphersuite::MLS_256_DHKEMP384_AES256GCM_SHA384_P384;

/// Returns true if `cs` is one of the two ciphersuites Lakko supports.
#[must_use]
pub fn is_supported(cs: Ciphersuite) -> bool {
    matches!(cs, DEFAULT | FIPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_set_is_exactly_two() {
        assert!(is_supported(DEFAULT));
        assert!(is_supported(FIPS));
        // Other valid MLS suites are explicitly rejected.
        assert!(!is_supported(
            Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519
        ));
        assert!(!is_supported(
            Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256
        ));
    }
}
