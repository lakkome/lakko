// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Word vault — the client side of ADR-0049's three-word recovery.
//!
//! Three words out of the twenty-four cannot derive the seed: BIP39 turns
//! 256 bits of entropy plus a checksum into 24 words of 11 bits each, so
//! three words are 33 bits of a 264-bit string. What three words CAN do is
//! stretch into a key that seals the 32-byte entropy, and a server that
//! holds the sealed entropy behind a guess counter can release it after a
//! correct answer. This module is everything the client computes; the
//! server stores bytes it cannot open and counts guesses (see
//! `services/auth/src/word_vault.rs`).
//!
//! ## Construction
//!
//! At enrollment, with the phrase in hand:
//!
//! ```text
//! positions   = three distinct 1-based indices into the 24 words, ascending
//! answer      = w[p1] " " w[p2] " " w[p3]           (lower-case, one space)
//! K           = Argon2id(answer, salt, params)
//! K_verify    = HKDF-SHA-256(K, info = "lakko/vault/verify/v1")
//! K_seal      = HKDF-SHA-256(K, info = "lakko/vault/seal/v1")
//! verifier    = SHA-256(K_verify)
//! sealed      = AEAD_seal(K_seal, entropy_32)
//! ```
//!
//! The server receives `positions`, `salt`, `params`, `verifier` and
//! `sealed`. At recovery the client rebuilds `K_verify` and `K_seal` from the
//! three typed words, sends `SHA-256(K_verify)` as its guess, and opens the
//! `sealed` bytes the server returns on a match. The **entropy** is what is
//! sealed, not the PBKDF2 seed: [`Mnemonic::from_entropy`] rebuilds the whole
//! phrase from it, and the seed does not reverse.
//!
//! ## What the server learns
//!
//! Three positions, a salt, the cost parameters, a hash of a hash and a
//! ciphertext. Never the words, the key or the entropy. An operator who
//! ignores their own guess counter can brute-force the 2^33 answers offline
//! at one Argon2id evaluation each; that is the stated invariant-#2
//! weakening of ADR-0049 and the reason the cost parameters are a budget
//! rather than an interactive setting.

use lakko_crypto::aead;
use lakko_crypto::hash::sha256;
use lakko_crypto::kdf::{argon2id_kek_with_params, hkdf_sha256, Argon2Params};
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{Mnemonic, MnemonicError, WORD_COUNT};

/// How many words the user answers with.
pub const ANSWER_WORDS: usize = 3;

/// Salt length in bytes.
pub const SALT_LEN: usize = 16;

/// Verifier length in bytes (a SHA-256 digest).
pub const VERIFIER_LEN: usize = 32;

/// Three 1-based word positions, ascending and distinct, each in `1..=24`.
pub type Positions = [u8; ANSWER_WORDS];

const VERIFY_INFO: &[u8] = b"lakko/vault/verify/v1";
const SEAL_INFO: &[u8] = b"lakko/vault/seal/v1";
const PASSKEY_SEAL_INFO: &[u8] = b"lakko/passkey-vault/seal/v1";

/// Argon2id cost the vault enrolls with today.
///
/// The vault is opened a handful of times in an account's life, so it can
/// afford far more than the interactive backup setting. These values are a
/// starting budget, not a measurement: the ADR asks for one evaluation to
/// take on the order of a second on a mid-range phone, and the parameters
/// travel with every enrollment (`Enrollment::params`) so they can be raised
/// later without breaking what was sealed under the old values. `p = 1`
/// because the web client runs Argon2 single-threaded in wasm, where extra
/// lanes only add sequential work.
pub const DEFAULT_PARAMS: Argon2Params = Argon2Params {
    m_kib: 256 * 1024,
    t: 3,
    p: 1,
};

/// Everything that goes wrong on the client side of the vault.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The positions are not three distinct ascending values in `1..=24`.
    #[error("positions must be three distinct ascending values in 1..=24")]
    BadPositions,
    /// The answer does not have exactly three words.
    #[error("expected {ANSWER_WORDS} words")]
    WrongWordCount,
    /// An answer word is not in the BIP39 English list.
    #[error("a word is not in the recovery word list")]
    UnknownWord,
    /// The salt is not [`SALT_LEN`] bytes.
    #[error("salt must be {SALT_LEN} bytes")]
    BadSalt,
    /// The Argon2id / HKDF stretch refused the parameters.
    #[error("key derivation failed")]
    Kdf,
    /// Sealing the entropy failed.
    #[error("sealing the entropy failed")]
    Seal,
    /// The sealed entropy did not open under this answer.
    #[error("the words did not open the vault")]
    Open,
    /// The opened bytes did not rebuild a mnemonic.
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
}

/// What the client sends to `POST /vault/enroll`.
#[derive(Debug, Clone)]
pub struct Enrollment {
    /// The three positions the user will be asked for.
    pub positions: Positions,
    /// Random salt, stored server-side and served back at recovery.
    pub salt: [u8; SALT_LEN],
    /// Cost parameters, stored beside the ciphertext.
    pub params: Argon2Params,
    /// `SHA-256(K_verify)`; the server compares a guess against it.
    pub verifier: [u8; VERIFIER_LEN],
    /// The 32-byte entropy sealed under `K_seal`.
    pub sealed_entropy: Vec<u8>,
}

/// The two keys a three-word answer yields. The verifier goes to the server
/// as the guess; the seal key stays on the client and opens what the server
/// returns.
pub struct AnswerKeys {
    verifier: [u8; VERIFIER_LEN],
    seal_key: SecretBytes<32>,
}

impl AnswerKeys {
    /// The guess to send: `SHA-256(K_verify)`.
    #[must_use]
    pub const fn verifier(&self) -> &[u8; VERIFIER_LEN] {
        &self.verifier
    }

    /// Open the sealed entropy the server returned and rebuild the mnemonic.
    ///
    /// # Errors
    ///
    /// [`VaultError::Open`] when the bytes do not authenticate under this
    /// answer (a wrong word, or a tampered blob), [`VaultError::Mnemonic`]
    /// when the opened bytes are not 32-byte entropy.
    pub fn open(&self, sealed_entropy: &[u8]) -> Result<Mnemonic, VaultError> {
        let entropy = Zeroizing::new(
            aead::open_with_key(self.seal_key.as_bytes(), sealed_entropy)
                .map_err(|_| VaultError::Open)?,
        );
        Ok(Mnemonic::from_entropy(&entropy)?)
    }
}

impl core::fmt::Debug for AnswerKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AnswerKeys")
            .field("verifier", &self.verifier)
            .field("seal_key", &"<redacted>")
            .finish()
    }
}

/// Are these three distinct ascending positions in `1..=24`?
#[must_use]
pub fn positions_are_valid(positions: &Positions) -> bool {
    let in_range = positions
        .iter()
        .all(|&p| (1..=u8::try_from(WORD_COUNT).unwrap_or(u8::MAX)).contains(&p));
    in_range && positions[0] < positions[1] && positions[1] < positions[2]
}

/// Draw three distinct positions uniformly at random and return them sorted.
#[must_use]
pub fn random_positions() -> Positions {
    let count = u8::try_from(WORD_COUNT).unwrap_or(u8::MAX);
    let mut chosen: Positions = [0; ANSWER_WORDS];
    let mut filled = 0;
    let mut byte = [0u8; 1];
    while filled < ANSWER_WORDS {
        random::fill(&mut byte);
        // Rejection sampling: 256 is not a multiple of 24, so drop the tail
        // that would bias the low values (240 = 10 * 24).
        if byte[0] >= 240 {
            continue;
        }
        let candidate = byte[0] % count + 1;
        if chosen[..filled].contains(&candidate) {
            continue;
        }
        chosen[filled] = candidate;
        filled += 1;
    }
    chosen.sort_unstable();
    chosen
}

/// Draw a random salt.
#[must_use]
pub fn random_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    random::fill(&mut salt);
    salt
}

/// Enroll a mnemonic: seal its entropy under the words at `positions`.
///
/// # Errors
///
/// [`VaultError::BadPositions`] for malformed positions; [`VaultError::Kdf`]
/// or [`VaultError::Seal`] when the primitives refuse (only possible with
/// out-of-range `params`).
pub fn enroll(
    mnemonic: &Mnemonic,
    positions: Positions,
    salt: [u8; SALT_LEN],
    params: Argon2Params,
) -> Result<Enrollment, VaultError> {
    if !positions_are_valid(&positions) {
        return Err(VaultError::BadPositions);
    }
    let words = mnemonic.words();
    let picked: Vec<&str> = positions
        .iter()
        .map(|&p| words[usize::from(p) - 1].as_str())
        .collect();
    let keys = derive_keys(&picked, &salt, params)?;
    let entropy = mnemonic.to_entropy();
    let sealed_entropy = aead::seal_with_key(keys.seal_key.as_bytes(), entropy.as_ref())
        .map_err(|_| VaultError::Seal)?;
    Ok(Enrollment {
        positions,
        salt,
        params,
        verifier: keys.verifier,
        sealed_entropy,
    })
}

/// Turn a typed three-word answer into the guess verifier and the seal key.
///
/// Words are trimmed and lower-cased; each must be in the BIP39 English list
/// so a typo is refused here rather than spent as a server-side guess.
///
/// # Errors
///
/// [`VaultError::WrongWordCount`], [`VaultError::UnknownWord`],
/// [`VaultError::BadSalt`], or [`VaultError::Kdf`].
pub fn answer(words: &[&str], salt: &[u8], params: Argon2Params) -> Result<AnswerKeys, VaultError> {
    if words.len() != ANSWER_WORDS {
        return Err(VaultError::WrongWordCount);
    }
    let salt: [u8; SALT_LEN] = salt.try_into().map_err(|_| VaultError::BadSalt)?;
    let normalized: Vec<String> = words.iter().map(|w| w.trim().to_lowercase()).collect();
    let list = bip39::Language::English;
    if normalized.iter().any(|w| list.find_word(w).is_none()) {
        return Err(VaultError::UnknownWord);
    }
    let picked: Vec<&str> = normalized.iter().map(String::as_str).collect();
    derive_keys(&picked, &salt, params)
}

/// Seal the mnemonic's entropy under a 32-byte secret a passkey's WebAuthn
/// PRF extension produced (ADR-0049 section 3). The PRF output is stable
/// per credential and follows a synced passkey to a new device, so the
/// sealed blob, stored opaque server-side, lets that passkey carry the
/// identity. HKDF domain-separates the seal key from the PRF output.
///
/// # Errors
///
/// [`VaultError::Kdf`] / [`VaultError::Seal`] when the primitives refuse
/// (not reachable with a 32-byte input).
pub fn seal_entropy_under_secret(
    mnemonic: &Mnemonic,
    secret: &[u8; 32],
) -> Result<Vec<u8>, VaultError> {
    let seal_key: SecretBytes<32> =
        hkdf_sha256(secret, &[], PASSKEY_SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    let entropy = mnemonic.to_entropy();
    aead::seal_with_key(seal_key.as_bytes(), entropy.as_ref()).map_err(|_| VaultError::Seal)
}

/// Inverse of [`seal_entropy_under_secret`].
///
/// # Errors
///
/// [`VaultError::Open`] when the bytes do not authenticate under this
/// secret; [`VaultError::Mnemonic`] when the opened bytes are not 32-byte
/// entropy.
pub fn open_entropy_under_secret(secret: &[u8; 32], sealed: &[u8]) -> Result<Mnemonic, VaultError> {
    let seal_key: SecretBytes<32> =
        hkdf_sha256(secret, &[], PASSKEY_SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    let entropy = Zeroizing::new(
        aead::open_with_key(seal_key.as_bytes(), sealed).map_err(|_| VaultError::Open)?,
    );
    Ok(Mnemonic::from_entropy(&entropy)?)
}

fn derive_keys(
    words: &[&str],
    salt: &[u8; SALT_LEN],
    params: Argon2Params,
) -> Result<AnswerKeys, VaultError> {
    let answer = Zeroizing::new(words.join(" "));
    let k =
        argon2id_kek_with_params(answer.as_bytes(), salt, params).map_err(|_| VaultError::Kdf)?;
    let k_verify: SecretBytes<32> =
        hkdf_sha256(k.as_bytes(), &[], VERIFY_INFO).map_err(|_| VaultError::Kdf)?;
    let seal_key: SecretBytes<32> =
        hkdf_sha256(k.as_bytes(), &[], SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    Ok(AnswerKeys {
        verifier: sha256(k_verify.as_bytes()),
        seal_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest cost Argon2 accepts, so the suite runs in milliseconds;
    /// the derivation is parameter-agnostic.
    const FAST: Argon2Params = Argon2Params {
        m_kib: 8,
        t: 1,
        p: 1,
    };

    fn picked(m: &Mnemonic, positions: Positions) -> Vec<String> {
        let words = m.words();
        positions
            .iter()
            .map(|&p| words[usize::from(p) - 1].clone())
            .collect()
    }

    #[test]
    fn enroll_then_answer_round_trips_the_mnemonic() {
        let m = Mnemonic::generate();
        let positions = random_positions();
        let e = enroll(&m, positions, random_salt(), FAST).unwrap();
        assert_eq!(e.positions, positions);
        assert_eq!(e.params, FAST);

        let words = picked(&m, positions);
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let keys = answer(&refs, &e.salt, e.params).unwrap();
        assert_eq!(keys.verifier(), &e.verifier);
        let back = keys.open(&e.sealed_entropy).unwrap();
        assert_eq!(back.phrase(), m.phrase());
    }

    #[test]
    fn a_wrong_word_gives_a_different_verifier_and_does_not_open() {
        let m = Mnemonic::generate();
        let positions: Positions = [2, 9, 17];
        let e = enroll(&m, positions, random_salt(), FAST).unwrap();
        let mut words = picked(&m, positions);
        words[1] = if words[1] == "zoo" {
            "abandon".into()
        } else {
            "zoo".into()
        };
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let keys = answer(&refs, &e.salt, e.params).unwrap();
        assert_ne!(keys.verifier(), &e.verifier);
        assert!(matches!(
            keys.open(&e.sealed_entropy),
            Err(VaultError::Open)
        ));
    }

    #[test]
    fn answer_normalizes_case_and_whitespace() {
        let m = Mnemonic::generate();
        let positions: Positions = [1, 12, 24];
        let e = enroll(&m, positions, random_salt(), FAST).unwrap();
        let words = picked(&m, positions);
        let shouted: Vec<String> = words
            .iter()
            .map(|w| format!("  {}  ", w.to_uppercase()))
            .collect();
        let refs: Vec<&str> = shouted.iter().map(String::as_str).collect();
        let keys = answer(&refs, &e.salt, e.params).unwrap();
        assert_eq!(keys.verifier(), &e.verifier);
    }

    #[test]
    fn answer_refuses_unknown_words_and_wrong_counts() {
        let salt = random_salt();
        assert!(matches!(
            answer(&["abandon", "notaword", "zoo"], &salt, FAST),
            Err(VaultError::UnknownWord)
        ));
        assert!(matches!(
            answer(&["abandon", "zoo"], &salt, FAST),
            Err(VaultError::WrongWordCount)
        ));
        assert!(matches!(
            answer(&["abandon", "ability", "zoo"], &salt[..8], FAST),
            Err(VaultError::BadSalt)
        ));
    }

    #[test]
    fn positions_validation() {
        assert!(positions_are_valid(&[1, 2, 3]));
        assert!(positions_are_valid(&[5, 14, 24]));
        assert!(!positions_are_valid(&[0, 1, 2]));
        assert!(!positions_are_valid(&[1, 1, 2]));
        assert!(!positions_are_valid(&[3, 2, 1]));
        assert!(!positions_are_valid(&[1, 2, 25]));
        let m = Mnemonic::generate();
        assert!(matches!(
            enroll(&m, [3, 2, 1], random_salt(), FAST),
            Err(VaultError::BadPositions)
        ));
    }

    #[test]
    fn random_positions_are_valid_and_spread() {
        let mut seen = [false; WORD_COUNT + 1];
        for _ in 0..500 {
            let p = random_positions();
            assert!(positions_are_valid(&p));
            for i in p {
                seen[usize::from(i)] = true;
            }
        }
        assert!(
            seen[1..].iter().all(|&s| s),
            "every position should appear in 500 draws"
        );
    }

    #[test]
    fn the_same_answer_and_salt_give_the_same_verifier_across_enrollments() {
        let m = Mnemonic::generate();
        let salt = random_salt();
        let a = enroll(&m, [4, 8, 15], salt, FAST).unwrap();
        let b = enroll(&m, [4, 8, 15], salt, FAST).unwrap();
        assert_eq!(a.verifier, b.verifier);
        // The AEAD nonce is fresh per seal, so the ciphertexts differ.
        assert_ne!(a.sealed_entropy, b.sealed_entropy);
    }

    #[test]
    fn the_verifier_and_the_seal_key_are_distinct_derivations() {
        let salt = random_salt();
        let keys = answer(&["abandon", "ability", "able"], &salt, FAST).unwrap();
        assert_ne!(keys.verifier(), keys.seal_key.as_bytes());
        assert_ne!(&sha256(keys.seal_key.as_bytes()), keys.verifier());
    }

    #[test]
    fn passkey_seal_round_trips_and_refuses_another_secret() {
        let m = Mnemonic::generate();
        let secret = [0x77u8; 32];
        let sealed = seal_entropy_under_secret(&m, &secret).unwrap();
        assert_eq!(
            open_entropy_under_secret(&secret, &sealed)
                .unwrap()
                .phrase(),
            m.phrase()
        );
        assert!(matches!(
            open_entropy_under_secret(&[0x78u8; 32], &sealed),
            Err(VaultError::Open)
        ));
        // A PRF output is never used as the seal key directly.
        assert!(aead::open_with_key(&secret, &sealed).is_err());
    }
}
