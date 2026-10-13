// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! 12-word English BIP39 mnemonic.
//!
//! `bip39::Mnemonic` (upstream) is generic over word count and
//! language. We fix both at this layer:
//!
//!   * Word count — 12 (128 bits of entropy, ADR-0072).
//!   * Language — English (the only widely-deployed wordlist;
//!     mixing languages across signup / recovery is a foot-gun).
//!
//! Each [`Mnemonic`] wraps a [`SecretMnemonic`] phrase string;
//! the `Debug` impl elides the contents.

use bip39::{Language, Mnemonic as InnerMnemonic};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::MnemonicError;
use crate::secret::SecretMnemonic;

/// Lakko fixes the mnemonic length at 12 words (ADR-0072).
pub const WORD_COUNT: usize = 12;

/// Length in bytes of the BIP39 PBKDF2 seed.
const SEED_LEN: usize = 64;

/// Length in bytes of the BIP39 raw entropy for a 12-word
/// mnemonic. 12 words × 11 bits/word = 132 bits, minus a 4-bit
/// checksum = 128 bits = 16 bytes (ADR-0072). Exposed publicly because the
/// passkey vault (ADR-0072) seals this exact buffer (the entropy is
/// the canonical "secret to recover", not the seed — the seed
/// is derived from the entropy via PBKDF2 with a fixed empty
/// passphrase).
pub const ENTROPY_LEN: usize = 16;

/// Owned 12-word English mnemonic.
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
    /// Generate a fresh 12-word mnemonic from the OS CSPRNG.
    ///
    /// On wasm32 the CSPRNG routes through WebCrypto via
    /// `getrandom/js` (configured in this crate's Cargo.toml).
    #[must_use]
    pub fn generate() -> Self {
        // `Mnemonic::generate(words)` may, in theory, fail if
        // `words` is not in {12,15,18,21,24}. 12 is in the set,
        // so the only remaining failure path is the OS CSPRNG —
        // which a sane platform never refuses. We fold both into
        // an `expect` and document the precondition.
        let inner = InnerMnemonic::generate_in(Language::English, WORD_COUNT)
            .expect("BIP39 generate(12) is always valid");
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
    /// * [`MnemonicError::WrongWordCount`] — fewer or more than 12
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

    /// The mnemonic as a 12-word, single-space-separated string.
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

    /// The 12 words as a `Vec<String>` — useful when the UI wants
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

    /// Extract the 16-byte BIP39 entropy (without the seed's
    /// PBKDF2 derivation). The passkey vault (ADR-0072) seals this
    /// buffer; the entropy plus the BIP39 checksum uniquely
    /// reconstructs the mnemonic via [`Mnemonic::from_entropy`].
    ///
    /// The returned [`SecretEntropy`] zeroizes on drop, same
    /// posture as [`SecretSeed`].
    #[must_use]
    pub fn to_entropy(&self) -> SecretEntropy {
        let raw = self.inner.to_entropy();
        debug_assert_eq!(
            raw.len(),
            ENTROPY_LEN,
            "BIP39 12-word mnemonic always has 16 bytes of entropy",
        );
        let mut arr = [0u8; ENTROPY_LEN];
        // `to_entropy()` returns a fresh `Vec<u8>` — copy into
        // the array so the heap allocation goes away with `raw`
        // (which is not zeroize-on-drop on its own).
        arr.copy_from_slice(&raw);
        SecretEntropy(arr)
    }

    /// Reconstruct the mnemonic from raw 16-byte entropy. The
    /// BIP39 checksum is re-computed at this layer; the
    /// returned [`Mnemonic`] is byte-identical to the original
    /// that produced the entropy via [`Mnemonic::to_entropy`].
    ///
    /// # Errors
    ///
    /// * [`MnemonicError::InvalidPhrase`] — entropy is the
    ///   wrong length for a 12-word mnemonic (must be exactly
    ///   [`ENTROPY_LEN`] = 16 bytes).
    pub fn from_entropy(entropy: &[u8]) -> Result<Self, MnemonicError> {
        if entropy.len() != ENTROPY_LEN {
            return Err(MnemonicError::InvalidPhrase);
        }
        let inner = InnerMnemonic::from_entropy_in(Language::English, entropy)
            .map_err(|_| MnemonicError::InvalidPhrase)?;
        Ok(Self { inner })
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

/// Owned 16-byte BIP39 entropy buffer that zeroizes on drop.
///
/// `Debug` formats the length only — the bytes never enter logs.
/// Sealed by the passkey vault (ADR-0072): the entropy is the
/// canonical "secret to recover", and BIP39's `from_entropy`
/// rebuilds the 12-word mnemonic byte-for-byte from it.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretEntropy(pub(crate) [u8; ENTROPY_LEN]);

impl SecretEntropy {
    /// Length in bytes (always 16).
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

impl AsRef<[u8]> for SecretEntropy {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for SecretEntropy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecretEntropy")
            .field("len", &ENTROPY_LEN)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_12_words() {
        let m = Mnemonic::generate();
        assert_eq!(m.words().len(), WORD_COUNT);
        assert_eq!(m.phrase().split_whitespace().count(), WORD_COUNT);
    }

    #[test]
    fn generate_is_random() {
        // Two consecutive generations should not collide. The
        // probability of an accidental match is 2^-128, well
        // below any empirical test threshold.
        let a = Mnemonic::generate().phrase();
        let b = Mnemonic::generate().phrase();
        assert_ne!(a, b);
    }

    #[test]
    fn from_phrase_round_trips_a_known_vector() {
        // BIP39 test vector — 12 words, English, all-zeros entropy.
        // Source: github.com/trezor/python-mnemonic/blob/master/vectors.json
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon about";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        assert_eq!(m.phrase(), phrase);
    }

    #[test]
    fn from_phrase_normalises_whitespace() {
        // Newlines, tabs, double-spaces — all collapsed.
        let phrase = "abandon  abandon\tabandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon\nabout";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        let canonical = "abandon abandon abandon abandon abandon abandon abandon abandon \
                         abandon abandon abandon about";
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
        // 12 tokens but one is gibberish.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon zzzzzzzz";
        let err = Mnemonic::from_phrase(phrase).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
    }

    #[test]
    fn from_phrase_rejects_bad_checksum() {
        // 12 valid wordlist tokens but the last one breaks the
        // 4-bit checksum baked into BIP39 entropy. Replacing the
        // canonical last word "about" with another wordlist entry
        // ("abandon") flips the checksum.
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon";
        let err = Mnemonic::from_phrase(phrase).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
    }

    #[test]
    fn to_seed_is_64_bytes_and_deterministic() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon about";
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
                        abandon abandon abandon about";
        let m_a = Mnemonic::from_phrase(phrase_a).expect("parse a");
        let m_b = Mnemonic::generate();
        let seed_a = m_a.to_seed();
        let seed_b = m_b.to_seed();
        assert_ne!(seed_a.as_ref(), seed_b.as_ref());
    }

    #[test]
    fn to_entropy_is_16_bytes_and_deterministic() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon about";
        let m = Mnemonic::from_phrase(phrase).expect("parse");
        let entropy_a = m.to_entropy();
        let entropy_b = m.to_entropy();
        assert_eq!(entropy_a.len(), ENTROPY_LEN);
        assert_eq!(entropy_a.as_ref(), entropy_b.as_ref());
        // The all-`abandon ... about` vector is the canonical
        // "all-zeros entropy" mnemonic in BIP39 conformance
        // suites — assert that here so an accidental BIP39 lib
        // bump that changed the wordlist endianness surfaces
        // immediately rather than silently breaking recovery.
        assert_eq!(entropy_a.as_ref(), &[0u8; ENTROPY_LEN]);
    }

    #[test]
    fn from_entropy_round_trips_with_to_entropy() {
        // Extracting + reconstructing the entropy must produce
        // the byte-identical mnemonic — that's the contract the
        // passkey vault (ADR-0072) relies on.
        let original = Mnemonic::generate();
        let entropy = original.to_entropy();
        let restored = Mnemonic::from_entropy(entropy.as_ref()).expect("rebuild");
        assert_eq!(original.phrase(), restored.phrase());
        assert_eq!(
            original.to_seed().as_ref(),
            restored.to_seed().as_ref(),
            "the BIP39 PBKDF2 seed must match too — recovery checks the seed, not the phrase string",
        );
    }

    #[test]
    fn from_entropy_rejects_wrong_length() {
        // 15 bytes — one short of the 12-word entropy length.
        let short = [0u8; ENTROPY_LEN - 1];
        let err = Mnemonic::from_entropy(&short).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
        // 17 bytes — one over.
        let long = [0u8; ENTROPY_LEN + 1];
        let err = Mnemonic::from_entropy(&long).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
        // Zero bytes.
        let err = Mnemonic::from_entropy(&[]).expect_err("must fail");
        assert!(matches!(err, MnemonicError::InvalidPhrase));
    }

    #[test]
    fn from_entropy_accepts_any_16_byte_buffer() {
        // Every 16-byte buffer is a valid BIP39 entropy — the
        // 4-bit checksum is computed by the constructor, not
        // looked up. A caller that feeds it the wrong 16 bytes
        // gets a well-formed mnemonic whose seed will not
        // OPAQUE-authenticate against the server, which surfaces
        // as an auth error downstream.
        let entropy = [0xABu8; ENTROPY_LEN];
        let m = Mnemonic::from_entropy(&entropy).expect("16 bytes is always valid");
        assert_eq!(m.to_entropy().as_ref(), &entropy);
    }

    // BIP39 conformance to the upstream reference vectors is the
    // responsibility of the `bip39` crate's own test suite — we
    // would only be re-running their tests if we hardcoded a
    // vector here. The wrapper-side tests above prove the contract
    // we actually offer (12 words, deterministic seed, the right
    // length, phrase round-trips, error mapping).
}
