// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-mnemonic`.
//!
//! Mirror of `lakko-ffi-wasm/src/mnemonic.rs`. Exposes a single
//! class, [`RecoveryMnemonic`], with two static factories
//! (`generate` / `from_phrase`) and three accessors (`phrase`,
//! `words`, `to_seed`).
//!
//! The signup flow (1.15d4) generates a fresh mnemonic, shows the
//! 24 words to the user, and immediately derives a 64-byte BIP39
//! seed that is fed into [`OpaqueRegister::start_recovery`](crate::auth::OpaqueRegister::start_recovery)
//! — enrolling a parallel OPAQUE recovery envelope at account-
//! creation time.

use lakko_mnemonic::{Mnemonic, MnemonicError};

use crate::FfiError;

impl From<MnemonicError> for FfiError {
    fn from(e: MnemonicError) -> Self {
        Self::Mnemonic(e.to_string())
    }
}

/// Recovery 24-word English BIP39 mnemonic.
///
/// Kotlin usage:
///
/// ```kotlin
/// import uniffi.lakko_ffi_uniffi.RecoveryMnemonic
///
/// // Signup: generate fresh words.
/// val m = RecoveryMnemonic.generate()
/// val words = m.words() // List<String> of size 24
/// // Display `words` once. Never persist or transmit.
/// val seed = m.toSeed() // ByteArray(64) — feed to OpaqueRegister.startRecovery
///
/// // Recovery: parse the user's typed phrase.
/// val m = RecoveryMnemonic.fromPhrase(typedPhrase) // throws on bad input
/// ```
#[derive(uniffi::Object)]
pub struct RecoveryMnemonic {
    inner: Mnemonic,
}

#[uniffi::export]
impl RecoveryMnemonic {
    /// Generate a fresh 24-word mnemonic from the OS CSPRNG.
    ///
    /// Infallible per BIP39 (24 is a valid word count and the OS
    /// RNG never refuses on a sane platform).
    #[uniffi::constructor]
    pub fn generate() -> Self {
        Self {
            inner: Mnemonic::generate(),
        }
    }

    /// Parse a typed phrase back into a [`RecoveryMnemonic`].
    ///
    /// Trims and collapses whitespace before splitting; the UI is
    /// forgiving about how the user pastes the words.
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::Mnemonic`] if the phrase has the wrong
    /// word count, contains a non-wordlist token, or fails the
    /// BIP39 checksum. The two security-sensitive cases (unknown
    /// word vs bad checksum) share the same error message to
    /// preserve BIP39's "no probing" property.
    #[uniffi::constructor]
    pub fn from_phrase(phrase: String) -> Result<Self, FfiError> {
        let inner = Mnemonic::from_phrase(&phrase)?;
        Ok(Self { inner })
    }

    /// The mnemonic as a single-space-separated string.
    pub fn phrase(&self) -> String {
        self.inner.phrase()
    }

    /// The 24 words as a list — convenient for rendering a
    /// numbered grid in the UI.
    pub fn words(&self) -> Vec<String> {
        self.inner.words()
    }

    /// Derive the 64-byte BIP39 PBKDF2 seed.
    ///
    /// Lakko calls this with an empty BIP39 passphrase —
    /// recovery must work with the words alone, so no
    /// `passphrase` argument is exposed at the FFI. Hand the
    /// returned bytes directly to
    /// [`OpaqueRegister.startRecovery`](crate::auth::OpaqueRegister::start_recovery).
    pub fn to_seed(&self) -> Vec<u8> {
        self.inner.to_seed().as_ref().to_vec()
    }

    /// Extract the 32-byte BIP39 entropy. Phase 1.21c social-
    /// recovery splits this exact buffer via
    /// [`crate::shamir::split`]; the 32-byte input recovers and
    /// re-builds the mnemonic via [`RecoveryMnemonic::from_entropy`].
    pub fn to_entropy(&self) -> Vec<u8> {
        self.inner.to_entropy().as_ref().to_vec()
    }
}

#[uniffi::export]
impl RecoveryMnemonic {
    /// Reconstruct a mnemonic from raw 32-byte entropy. Inverse
    /// of [`RecoveryMnemonic::to_entropy`].
    ///
    /// # Errors
    ///
    /// Returns [`FfiError::Mnemonic`] when `entropy` is not
    /// exactly 32 bytes. Every 32-byte buffer is a valid BIP39
    /// entropy (the 8-bit checksum is computed at the
    /// constructor, not verified), so the wrong-length path is
    /// the only failure mode.
    #[uniffi::constructor]
    pub fn from_entropy(entropy: Vec<u8>) -> Result<Self, FfiError> {
        let inner = Mnemonic::from_entropy(&entropy)?;
        Ok(Self { inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_returns_24_words() {
        let m = RecoveryMnemonic::generate();
        let words = m.words();
        assert_eq!(words.len(), 24);
        assert!(!m.phrase().is_empty());
    }

    #[test]
    fn to_seed_is_64_bytes() {
        let m = RecoveryMnemonic::generate();
        let seed = m.to_seed();
        assert_eq!(seed.len(), 64);
    }

    #[test]
    fn from_phrase_round_trips() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = RecoveryMnemonic::from_phrase(phrase.to_string()).expect("parse");
        assert_eq!(m.phrase(), phrase);
        assert_eq!(m.words().len(), 24);
    }

    #[test]
    fn to_entropy_is_32_bytes_and_round_trips_via_from_entropy() {
        let original = RecoveryMnemonic::generate();
        let entropy = original.to_entropy();
        assert_eq!(entropy.len(), 32);
        let restored = RecoveryMnemonic::from_entropy(entropy).expect("rebuild");
        assert_eq!(original.phrase(), restored.phrase());
    }

    #[test]
    fn from_entropy_rejects_wrong_length() {
        match RecoveryMnemonic::from_entropy(vec![0u8; 31]) {
            Err(FfiError::Mnemonic(_)) => {},
            Err(other) => panic!("expected Mnemonic error, got {other:?}"),
            Ok(_) => panic!("31 bytes must fail"),
        }
    }

    #[test]
    fn from_phrase_rejects_bad_checksum() {
        // 24 valid words but the last one breaks the checksum.
        // `expect_err` would need `Debug` on the `Object`-typed Ok
        // variant — match instead.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon";
        match RecoveryMnemonic::from_phrase(phrase.to_string()) {
            Err(FfiError::Mnemonic(_)) => {},
            Err(other) => panic!("expected Mnemonic error, got {other:?}"),
            Ok(_) => panic!("bad checksum must fail"),
        }
    }
}
