// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-mnemonic`.
//!
//! Exposes a single class, `RecoveryMnemonic`, with two static
//! factories (`generate` / `fromPhrase`) and three accessors
//! (`phrase`, `words`, `toSeed`). The shape matches the Rust
//! [`Mnemonic`] 1:1.
//!
//! Phase 1.16c3c1 wires the JS surface; the apps/web signup flow
//! consumes it to enrol a parallel OPAQUE recovery envelope at
//! account-creation time.

use lakko_mnemonic::{Mnemonic, MnemonicError};
use wasm_bindgen::prelude::*;

/// Map a `lakko_mnemonic::MnemonicError` to a `JsError`. Same
/// elision policy as the underlying error: we keep the message
/// short and avoid hinting at which word failed (don't help an
/// attacker probe the wordlist).
fn mnemonic_err(e: MnemonicError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// JS-facing 24-word English BIP39 mnemonic.
///
/// JS usage:
///
/// ```js
/// import { RecoveryMnemonic } from "@lakko/ffi-wasm";
///
/// // Signup: generate fresh words.
/// const m = RecoveryMnemonic.generate();
/// const words = m.words; // string[24]
/// // Display `words` once. Never persist or transmit.
/// const seed = m.toSeed(); // Uint8Array(64) — feed to OpaqueRegister.startRecovery
///
/// // Recovery: parse the user's typed phrase.
/// const m = RecoveryMnemonic.fromPhrase(typedPhrase); // throws on bad input
/// ```
#[wasm_bindgen]
pub struct RecoveryMnemonic {
    inner: Mnemonic,
}

impl RecoveryMnemonic {
    /// Borrow the wrapped mnemonic (crate-internal: the word vault seals its
    /// entropy without round-tripping through JS).
    pub(crate) const fn inner(&self) -> &Mnemonic {
        &self.inner
    }

    /// Wrap a mnemonic the crate rebuilt itself (the word vault's `open`).
    pub(crate) const fn from_inner(inner: Mnemonic) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl RecoveryMnemonic {
    /// Generate a fresh 24-word mnemonic from the OS CSPRNG.
    ///
    /// On wasm32 the CSPRNG routes through WebCrypto via
    /// `getrandom/js`. The function is infallible per the BIP39
    /// spec (24 is a valid word count and the OS RNG never
    /// refuses on a sane platform).
    #[wasm_bindgen(js_name = generate)]
    pub fn generate() -> RecoveryMnemonic {
        Self {
            inner: Mnemonic::generate(),
        }
    }

    /// Parse a typed phrase back into a [`RecoveryMnemonic`].
    ///
    /// Trims and collapses whitespace before splitting; the
    /// browser is forgiving about how the user copy-pastes the
    /// words.
    ///
    /// # Errors
    ///
    /// Throws if the phrase has the wrong word count, contains a
    /// non-wordlist token, or fails the BIP39 checksum. The two
    /// security-sensitive cases (unknown word vs bad checksum)
    /// produce the same JS error message — preserving BIP39's
    /// "no probing" property.
    #[wasm_bindgen(js_name = fromPhrase)]
    pub fn from_phrase(phrase: &str) -> Result<RecoveryMnemonic, JsError> {
        Mnemonic::from_phrase(phrase)
            .map(|inner| Self { inner })
            .map_err(mnemonic_err)
    }

    /// The mnemonic as a single-space-separated string (24 words,
    /// English wordlist). Returns a fresh `String` per call.
    #[wasm_bindgen(getter)]
    pub fn phrase(&self) -> String {
        self.inner.phrase()
    }

    /// The 24 words as a JS array of strings — convenient for
    /// rendering a numbered grid in the UI.
    #[wasm_bindgen(getter)]
    pub fn words(&self) -> Vec<String> {
        self.inner.words()
    }

    /// Derive the 64-byte BIP39 PBKDF2 seed.
    ///
    /// Lakko intentionally calls this with an empty BIP39
    /// passphrase — recovery must work with the words alone, so
    /// no `passphrase` argument is exposed at the FFI. Hand the
    /// returned bytes directly to
    /// [`OpaqueRegister.startRecovery`](crate::auth::OpaqueRegister::start_recovery).
    #[wasm_bindgen(js_name = toSeed)]
    pub fn to_seed(&self) -> Vec<u8> {
        self.inner.to_seed().as_ref().to_vec()
    }

    /// Extract the 32-byte BIP39 entropy. Phase 1.21c social-
    /// recovery flow splits this exact buffer via
    /// [`crate::shamir::split`] — 32 bytes splits + recovers
    /// far more cheaply than the ~210-byte phrase string,
    /// and `fromEntropy` round-trips it back to the same
    /// mnemonic for the standard recovery path.
    #[wasm_bindgen(js_name = toEntropy)]
    pub fn to_entropy(&self) -> Vec<u8> {
        self.inner.to_entropy().as_ref().to_vec()
    }

    /// Reconstruct a mnemonic from raw 32-byte entropy. Inverse
    /// of [`RecoveryMnemonic::to_entropy`]; mirror of
    /// `lakko_mnemonic::Mnemonic::from_entropy`.
    ///
    /// # Errors
    ///
    /// `JsError` when `entropy` is not exactly 32 bytes — every
    /// 32-byte buffer is a valid BIP39 entropy (the 8-bit
    /// checksum is computed at the constructor, not verified),
    /// so this is the only failure path.
    #[wasm_bindgen(js_name = fromEntropy)]
    pub fn from_entropy(entropy: &[u8]) -> Result<RecoveryMnemonic, JsError> {
        let inner = lakko_mnemonic::Mnemonic::from_entropy(entropy)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(RecoveryMnemonic { inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generation produces 24 distinct words and a non-empty
    /// phrase string. Error paths are covered in the underlying
    /// `lakko-mnemonic` crate; we cannot exercise them on the
    /// host because `JsError::new` is a wasm-bindgen import
    /// stubbed off-wasm32 (same caveat as in `mls.rs`).
    #[test]
    fn generate_returns_24_words() {
        let m = RecoveryMnemonic::generate();
        let words = m.words();
        assert_eq!(words.len(), 24);
        assert!(!m.phrase().is_empty());
    }

    /// `toSeed()` returns 64 bytes for any well-formed mnemonic.
    #[test]
    fn to_seed_is_64_bytes() {
        let m = RecoveryMnemonic::generate();
        let seed = m.to_seed();
        assert_eq!(seed.len(), 64);
    }

    /// `fromPhrase()` round-trips a well-known 24-word vector.
    #[test]
    fn from_phrase_round_trips() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = RecoveryMnemonic::from_phrase(phrase).expect("parse");
        assert_eq!(m.phrase(), phrase);
        assert_eq!(m.words().len(), 24);
    }
}
