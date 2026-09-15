// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the ADR-0049 word vault
//! (`lakko_mnemonic::vault`).
//!
//! Two entry points and two handle classes:
//!
//! - `wordVaultEnroll(mnemonic) -> WordVaultEnrollment` draws random
//!   positions and a salt, stretches the three words at those positions
//!   with the default Argon2id budget, and seals the 32-byte entropy. The
//!   handle exposes exactly what `POST /vault/enroll` needs.
//! - `wordVaultAnswer(words, salt, mKib, t, p) -> WordVaultAnswer` turns a
//!   typed three-word answer into the guess `verifier`; `answer.open(sealed)`
//!   opens what the server returned into a `RecoveryMnemonic`. The seal key
//!   never leaves the handle.
//!
//! Argon2id at the default budget takes on the order of a second and runs
//! on the calling thread: callers should run it off the main thread or show
//! progress.

use lakko_crypto::kdf::Argon2Params;
use lakko_mnemonic::vault;
use wasm_bindgen::prelude::*;

use crate::mnemonic::RecoveryMnemonic;

fn vault_err(e: vault::VaultError) -> JsError {
    JsError::new(&e.to_string())
}

/// What `POST /vault/enroll` needs, as bytes and numbers.
#[wasm_bindgen]
pub struct WordVaultEnrollment {
    inner: vault::Enrollment,
}

#[wasm_bindgen]
impl WordVaultEnrollment {
    /// The three 1-based positions, ascending.
    #[wasm_bindgen(getter)]
    pub fn positions(&self) -> Vec<u8> {
        self.inner.positions.to_vec()
    }

    /// The 16-byte salt.
    #[wasm_bindgen(getter)]
    pub fn salt(&self) -> Vec<u8> {
        self.inner.salt.to_vec()
    }

    /// Argon2id memory cost in KiB.
    #[wasm_bindgen(getter, js_name = argon2MKib)]
    pub fn argon2_m_kib(&self) -> u32 {
        self.inner.params.m_kib
    }

    /// Argon2id passes.
    #[wasm_bindgen(getter, js_name = argon2T)]
    pub fn argon2_t(&self) -> u32 {
        self.inner.params.t
    }

    /// Argon2id lanes.
    #[wasm_bindgen(getter, js_name = argon2P)]
    pub fn argon2_p(&self) -> u32 {
        self.inner.params.p
    }

    /// The 32-byte verifier the server compares guesses against.
    #[wasm_bindgen(getter)]
    pub fn verifier(&self) -> Vec<u8> {
        self.inner.verifier.to_vec()
    }

    /// The sealed 32-byte entropy (AEAD envelope).
    #[wasm_bindgen(getter, js_name = sealedEntropy)]
    pub fn sealed_entropy(&self) -> Vec<u8> {
        self.inner.sealed_entropy.clone()
    }
}

/// The result of stretching a typed answer: the guess to send, and the
/// ability to open what comes back.
#[wasm_bindgen]
pub struct WordVaultAnswer {
    inner: vault::AnswerKeys,
}

#[wasm_bindgen]
impl WordVaultAnswer {
    /// The 32-byte guess to send to `POST /vault/recover/finish`.
    #[wasm_bindgen(getter)]
    pub fn verifier(&self) -> Vec<u8> {
        self.inner.verifier().to_vec()
    }

    /// Open the sealed entropy the server returned.
    ///
    /// # Errors
    ///
    /// Throws when the bytes do not open under this answer (a wrong word or
    /// a tampered blob).
    #[wasm_bindgen(js_name = open)]
    pub fn open(&self, sealed_entropy: &[u8]) -> Result<RecoveryMnemonic, JsError> {
        let inner = self.inner.open(sealed_entropy).map_err(vault_err)?;
        Ok(RecoveryMnemonic::from_inner(inner))
    }
}

/// Enroll a mnemonic into the word vault with fresh random positions, a
/// fresh salt and the default Argon2id budget.
///
/// # Errors
///
/// Throws only if the primitives refuse (not reachable with the default
/// parameters).
#[wasm_bindgen(js_name = wordVaultEnroll)]
pub fn word_vault_enroll(mnemonic: &RecoveryMnemonic) -> Result<WordVaultEnrollment, JsError> {
    let inner = vault::enroll(
        mnemonic.inner(),
        vault::random_positions(),
        vault::random_salt(),
        vault::DEFAULT_PARAMS,
    )
    .map_err(vault_err)?;
    Ok(WordVaultEnrollment { inner })
}

/// Stretch a typed three-word answer against the served salt and cost.
///
/// # Errors
///
/// Throws when a word is not in the BIP39 list, the count is not three, the
/// salt is not 16 bytes, or the cost parameters are out of range.
#[wasm_bindgen(js_name = wordVaultAnswer)]
pub fn word_vault_answer(
    words: Vec<String>,
    salt: &[u8],
    argon2_m_kib: u32,
    argon2_t: u32,
    argon2_p: u32,
) -> Result<WordVaultAnswer, JsError> {
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let params = Argon2Params {
        m_kib: argon2_m_kib,
        t: argon2_t,
        p: argon2_p,
    };
    let inner = vault::answer(&refs, salt, params).map_err(vault_err)?;
    Ok(WordVaultAnswer { inner })
}

/// Seal a mnemonic's entropy under a passkey's 32-byte PRF output
/// (ADR-0049 section 3). The result is the opaque blob `PUT /vault/passkey`
/// stores.
///
/// # Errors
///
/// Throws when `prf_output` is not 32 bytes or the primitives refuse.
#[wasm_bindgen(js_name = passkeyVaultSeal)]
pub fn passkey_vault_seal(
    prf_output: &[u8],
    mnemonic: &RecoveryMnemonic,
) -> Result<Vec<u8>, JsError> {
    let secret: [u8; 32] = prf_output
        .try_into()
        .map_err(|_| JsError::new("prf output must be 32 bytes"))?;
    vault::seal_entropy_under_secret(mnemonic.inner(), &secret).map_err(vault_err)
}

/// Open a passkey-vault blob with the same credential's PRF output.
///
/// # Errors
///
/// Throws when `prf_output` is not 32 bytes or the blob does not open
/// under it.
#[wasm_bindgen(js_name = passkeyVaultOpen)]
pub fn passkey_vault_open(prf_output: &[u8], sealed: &[u8]) -> Result<RecoveryMnemonic, JsError> {
    let secret: [u8; 32] = prf_output
        .try_into()
        .map_err(|_| JsError::new("prf output must be 32 bytes"))?;
    let inner = vault::open_entropy_under_secret(&secret, sealed).map_err(vault_err)?;
    Ok(RecoveryMnemonic::from_inner(inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_and_answer_round_trip_over_the_wasm_shape() {
        let m = RecoveryMnemonic::generate();
        let e = word_vault_enroll(&m).unwrap();
        assert_eq!(e.positions().len(), 3);
        assert_eq!(e.salt().len(), 16);
        let words = m.words();
        let picked: Vec<String> = e
            .positions()
            .iter()
            .map(|&p| words[usize::from(p) - 1].clone())
            .collect();
        let a = word_vault_answer(
            picked,
            &e.salt(),
            e.argon2_m_kib(),
            e.argon2_t(),
            e.argon2_p(),
        )
        .unwrap();
        assert_eq!(a.verifier(), e.verifier());
        let back = a.open(&e.sealed_entropy()).unwrap();
        assert_eq!(back.words(), words);
    }

    #[test]
    fn passkey_vault_round_trip_over_the_wasm_shape() {
        let m = RecoveryMnemonic::generate();
        let prf = [0x11u8; 32];
        let sealed = passkey_vault_seal(&prf, &m).unwrap();
        let back = passkey_vault_open(&prf, &sealed).unwrap();
        assert_eq!(back.words(), m.words());
        // The refusal paths (wrong PRF output, short input) construct a
        // `JsError`, which only exists on wasm32; they are covered by the
        // `lakko-mnemonic` vault tests.
    }
}
