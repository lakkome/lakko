// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The passkey vault (ADR-0072 decision 6): the recovery phrase's entropy
//! sealed under a passkey's WebAuthn PRF output.
//!
//! [`seal_entropy_under_secret`] seals the 16-byte phrase entropy under a
//! key derived from a passkey's 32-byte WebAuthn PRF output;
//! [`open_entropy_under_secret`] opens it and rebuilds the phrase with
//! [`Mnemonic::from_entropy`], which refuses anything but 16 bytes. The
//! server stores the blob opaque and cannot search it: there is no
//! verifier, and the key comes from the authenticator.
//!
//! The email vault (ADR-0075 section 2), which sealed the same entropy
//! under a key stretched from the login handle and the recovery email
//! address, was removed on 2026-11-26 (ADR-0075, amended the second time
//! that day): the operator sees the address and holds the salt, so the vault
//! was protected by server secrets only. A mailed code never returns keys.
//!
//! The three-word check is not here: it runs on the device against the
//! phrase the device holds and involves no server and no sealed value, see
//! [`crate::word_check`].

use lakko_crypto::aead;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{Mnemonic, MnemonicError};

const PASSKEY_SEAL_INFO: &[u8] = b"lakko/passkey-vault/seal/v1";

/// Everything that goes wrong sealing or opening the passkey vault.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VaultError {
    /// The HKDF step refused its parameters.
    #[error("key derivation failed")]
    Kdf,
    /// Sealing the entropy under the passkey secret failed.
    #[error("sealing the entropy failed")]
    Seal,
    /// The passkey blob did not open under this secret.
    #[error("the passkey did not open the stored keys")]
    Open,
    /// The opened bytes did not rebuild a mnemonic.
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
}

/// Seal the mnemonic's entropy under a 32-byte secret a passkey's WebAuthn
/// PRF extension produced (ADR-0049 section 3, ADR-0072 decision 6). The PRF output is stable
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
/// secret; [`VaultError::Mnemonic`] when the opened bytes are not 16-byte
/// entropy.
pub fn open_entropy_under_secret(secret: &[u8; 32], sealed: &[u8]) -> Result<Mnemonic, VaultError> {
    let seal_key: SecretBytes<32> =
        hkdf_sha256(secret, &[], PASSKEY_SEAL_INFO).map_err(|_| VaultError::Kdf)?;
    let entropy = Zeroizing::new(
        aead::open_with_key(seal_key.as_bytes(), sealed).map_err(|_| VaultError::Open)?,
    );
    Ok(Mnemonic::from_entropy(&entropy)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkey_seal_round_trips_and_refuses_another_secret() {
        let m = Mnemonic::generate();
        let secret = [0x77u8; 32];
        let sealed = seal_entropy_under_secret(&m, &secret).unwrap();
        // nonce(12) + entropy(16) + tag(16)
        assert_eq!(sealed.len(), 44);
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
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(matches!(
            open_entropy_under_secret(&secret, &tampered),
            Err(VaultError::Open)
        ));
        // A PRF output is never used as the seal key directly.
        assert!(aead::open_with_key(&secret, &sealed).is_err());
    }

    #[test]
    fn a_passkey_blob_holding_32_bytes_fails_closed() {
        // What a blob sealed for a 24-word phrase looked like: it
        // authenticates under the right secret and is still refused.
        let secret = [0x21u8; 32];
        let seal_key: SecretBytes<32> = hkdf_sha256(&secret, &[], PASSKEY_SEAL_INFO).unwrap();
        let old = aead::seal_with_key(seal_key.as_bytes(), &[0xABu8; 32]).unwrap();
        assert!(matches!(
            open_entropy_under_secret(&secret, &old),
            Err(VaultError::Mnemonic(_))
        ));
    }
}
