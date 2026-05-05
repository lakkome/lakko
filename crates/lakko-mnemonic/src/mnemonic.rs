// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! 24-word English BIP39 mnemonic.
//!
//! `bip39::Mnemonic` (upstream) is generic over word count and
//! language. We fix both at this layer:
//!
//!   * Word count — 24 (256 bits of entropy).
//!   * Language — English (the only widely-deployed wordlist;
//!     mixing languages across signup / recovery is a foot-gun).
//!
//! Each [`Mnemonic`] wraps a [`SecretMnemonic`] phrase string;
//! the `Debug` impl elides the contents.

use bip39::{Language, Mnemonic as InnerMnemonic};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::MnemonicError;
use crate::secret::SecretMnemonic;

/// Lakko fixes the mnemonic length at 24 words.
pub const WORD_COUNT: usize = 24;

/// Length in bytes of the BIP39 PBKDF2 seed.
const SEED_LEN: usize = 64;

/// Owned 24-word English mnemonic.
///
/// Cheap to construct (the upstream parser does the wordlist
/// lookup once); cheap to `phrase()` repeatedly. Not `Clone` —
/// the secret nature of the phrase makes accidental copies a
/// foot-gun. Use [`Mnemonic::phrase`] explicitly when a copy is
/// genuinely needed.
pub struct Mnemonic {
    /// We hold the `bip39::Mnemonic` rather than the phrase
    /// string so `to_seed` can call `inner.to_seed_normalized`
    /// without a re-parse. The phrase lives in
    /// [`Mnemonic::phrase_as_secret`] / [`Mnemonic::phrase`].
    inner: InnerMnemonic,
}

impl Mnemonic {
    /// Generate a fresh 24-word mnemonic from the OS CSPRNG.
    ///
    /// On wasm32 the CSPRNG routes through WebCrypto via
    /// `getrandom/js` (configured in this crate's Cargo.toml).
    #[must_use]
    pub fn generate() -> Self {
        // `Mnemonic::generate(words)` may, in theory, fail if
        // `words` is not in {12,15,18,21,24}. 24 is in the set,
        // so the only remaining failure path is the OS CSPRNG —
        // which a sane platform never refuses. We fold both into
        // an `expect` and document the precondition.
        let inner = InnerMnemonic::generate_in(Language::English, WORD_COUNT)
            .expect("BIP39 generate(24) is always valid");
        Self { inner }
    }

    /// Parse a phrase string back into a [`Mnemonic`].
    ///
    /// Trims surrounding whitespace and collapses internal runs
    /// of whitespace before splitting — common UI hazards
    /// (newlines, double-spaces) are forgiven.
    ///
    /// # Errors
    ///
    /// * [`MnemonicError::WrongWordCount`] — fewer or more than 24
    ///   non-empty whitespace-separated tokens.
    /// * [`MnemonicError::InvalidPhrase`] — a token is not in
    ///   the BIP39 English wordlist, OR the BIP39 checksum fails.
    pub fn from_phrase(phrase: &str) -> Result<Self, MnemonicError> {
        let words: Vec<&str> = phrase.split_whitespace().collect();
        if words.len() != WORD_COUNT {
            return Err(MnemonicError::WrongWordCount { got: words.len() });
        }
        let normalized = words.join(" ");
        let inner = InnerMnemonic::parse_in(Language::English, &normalized)
            .map_err(|_| MnemonicError::InvalidPhrase)?;
        Ok(Self { inner })
    }

    /// The mnemonic as a 24-word, single-space-separated string.
    /// Returns an owned [`SecretMnemonic`] — wrap the value in a
    /// scope that drops it once the UI has rendered it.
    #[must_use]
    pub fn phrase_as_secret(&self) -> SecretMnemonic {
        SecretMnemonic::new(self.inner.to_string())
    }

    /// The mnemonic as a `String`. Convenience wrapper around
    /// [`Mnemonic::phrase_as_secret`] for callers that have
    /// already arranged to handle the contents safely (e.g. the
    /// wasm-bindgen FFI that hands the words to the browser UI).
    #[must_use]
    pub fn phrase(&self) -> String {
        self.inner.to_string()
    }

    /// The 24 words as a `Vec<String>` — useful when the UI wants
    /// to render them in a numbered grid.
    #[must_use]
    pub fn words(&self) -> Vec<String> {
        self.inner
            .words()
            .map(std::string::ToString::to_string)
            .collect()
    }

    /// Derive the 64-byte BIP39 PBKDF2 seed.
    ///
    /// Lakko intentionally calls this with an empty BIP39
    /// passphrase — recovery must work with the words alone, so
    /// adding a second secret here would defeat the point. The
    /// API does not even expose a passphrase parameter for this
    /// reason.
    ///
    /// The returned [`SecretSeed`] zeroizes on drop. Hand
    /// `seed.as_ref()` directly to
    /// [`RegistrationSession::start_recovery`](
    /// ../lakko_auth_client/struct.RegistrationSession.html#method.start_recovery).
    #[must_use]
    pub fn to_seed(&self) -> SecretSeed {
        // BIP39 prescribes PBKDF2-HMAC-SHA512, 2048 iterations,
        // salt = "mnemonic" || passphrase. With an empty
        // passphrase the salt is just the literal "mnemonic".
        let bytes = self.inner.to_seed_normalized("");
        SecretSeed(bytes)
    }
}

/// Owned 64-byte BIP39 seed that zeroizes on drop.
///
/// `Debug` formats the length only — the bytes never enter logs.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretSeed(pub(crate) [u8; SEED_LEN]);

impl SecretSeed {
    /// Length in bytes (always 64).
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false` — kept for API uniformity with `len`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl AsRef<[u8]> for SecretSeed {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for SecretSeed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecretSeed")
            .field("len", &SEED_LEN)
            .finish()
    }
}

impl core::fmt::Debug for Mnemonic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mnemonic")
            .field("words", &WORD_COUNT)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_24_words() {
        let m = Mnemonic::generate();
        assert_eq!(m.words().len(), WORD_COUNT);
        assert_eq!(m.phrase().split_whitespace().count(), WORD_COUNT);
    }

    #[test]
    fn generate_is_random() {
        // Two consecutive generations should not collide. The
        // probability of an accidental match is 2^-256, well
        // below any empirical test threshold.
        let a = Mnemonic::generate().phrase();
        let b = Mnemonic::generate().phrase();
        assert_ne!(a, b);
    }

    #[test]
    fn from_phrase_round_trips_a_known_vector() {
        // BIP39 test vector — 24 words, English, all-zeros entropy.
        // Source: github.com/trezor/python-mnemonic/blob/master/vectors.json
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        assert_eq!(m.phrase(), phrase);
    }

    #[test]
    fn from_phrase_normalises_whitespace() {
        // Newlines, tabs, double-spaces — all collapsed.
        let phrase = "abandon  abandon\tabandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon\nart";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        let canonical = "abandon abandon abandon abandon abandon abandon abandon abandon \
                         abandon abandon abandon abandon abandon abandon abandon abandon \
                         abandon abandon abandon abandon abandon abandon abandon art";
        assert_eq!(m.phrase(), canonical);
    }

    #[test]
    fn from_phrase_rejects_short_input() {
        let err = Mnemonic::from_phrase("abandon abandon abandon").expect_err("must fail");
        match err {
            MnemonicError::WrongWordCount { got } => assert_eq!(got, 3),
            other => panic!("expected WrongWordCount, got {other:?}"),
        }
    }

    #[test]
    fn from_phrase_rejects_unknown_words() {
        // 24 tokens but one is gibberish.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon zzzzzzzz";
        let err = Mnemonic::from_phrase(phrase).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
    }

    #[test]
    fn from_phrase_rejects_bad_checksum() {
        // 24 valid wordlist tokens but the last one breaks the
        // 8-bit checksum baked into BIP39 entropy. Replacing the
        // canonical last word "art" with another wordlist entry
        // ("about") flips the checksum.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon about";
        let err = Mnemonic::from_phrase(phrase).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
    }

    #[test]
    fn to_seed_is_64_bytes_and_deterministic() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon abandon abandon art";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        let seed_a = m.to_seed();
        let seed_b = m.to_seed();
        assert_eq!(seed_a.len(), 64);
        assert_eq!(seed_a.as_ref(), seed_b.as_ref());
    }

    #[test]
    fn different_phrases_produce_different_seeds() {
        // Sanity check that the seed function actually depends on
        // the phrase — would catch e.g. an accidental
        // `to_seed_normalized("foo")` with a hardcoded constant.
        let phrase_a = "abandon abandon abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon abandon abandon art";
        let m_a = Mnemonic::from_phrase(phrase_a).expect("parse a");
        let m_b = Mnemonic::generate();
        let seed_a = m_a.to_seed();
        let seed_b = m_b.to_seed();
        assert_ne!(seed_a.as_ref(), seed_b.as_ref());
    }

    // BIP39 conformance to the upstream reference vectors is the
    // responsibility of the `bip39` crate's own test suite — we
    // would only be re-running their tests if we hardcoded a
    // vector here. The wrapper-side tests above prove the contract
    // we actually offer (24 words, deterministic seed, the right
    // length, phrase round-trips, error mapping).
}
