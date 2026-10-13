// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko_mnemonic::vault` (ADR-0072 decision 6): the
//! passkey vault. Mirror of `crates/lakko-ffi-wasm/src/passkey_vault.rs`.
//!
//! [`passkey_vault_seal`] seals the phrase entropy under a passkey's 32-byte
//! PRF output into the opaque blob `PUT /vault/passkey` stores;
//! [`passkey_vault_open`] opens the blob `POST /vault/passkey/get` returned.
//! The PRF output never leaves the device.

use std::sync::Arc;

use lakko_mnemonic::vault::{self, VaultError};

use crate::mnemonic::RecoveryMnemonic;
use crate::FfiError;

fn passkey_err(e: &VaultError) -> FfiError {
    let code = match e {
        VaultError::Open => return FfiError::PasskeyVaultNotOpened,
        VaultError::Mnemonic(_) => "bad-entropy",
        VaultError::Seal => "seal",
        _ => "kdf",
    };
    FfiError::PasskeyVault(format!("{code}: {e}"))
}

fn prf_secret(prf_output: &[u8]) -> Result<[u8; 32], FfiError> {
    prf_output.try_into().map_err(|_| {
        FfiError::PasskeyVault("bad-secret-length: the PRF output must be 32 bytes".into())
    })
}

/// Seal the mnemonic's entropy under a passkey's 32-byte PRF output. The
/// result is the opaque blob `PUT /vault/passkey` stores.
///
/// # Errors
///
/// [`FfiError::PasskeyVault`] when `prf_output` is not 32 bytes
/// (`bad-secret-length`) or the primitives refuse.
#[uniffi::export]
pub fn passkey_vault_seal(
    mnemonic: Arc<RecoveryMnemonic>,
    prf_output: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let secret = prf_secret(&prf_output)?;
    vault::seal_entropy_under_secret(mnemonic.inner(), &secret).map_err(|e| passkey_err(&e))
}

/// Open a passkey-vault blob with the same credential's PRF output and
/// rebuild the recovery phrase. The caller must still check the phrase
/// against the account's published identity before using it.
///
/// # Errors
///
/// [`FfiError::PasskeyVaultNotOpened`] when the blob does not open under
/// this output; [`FfiError::PasskeyVault`] when `prf_output` is not 32 bytes
/// (`bad-secret-length`) or the opened bytes are not 16-byte entropy
/// (`bad-entropy`).
#[uniffi::export]
pub fn passkey_vault_open(
    prf_output: Vec<u8>,
    sealed: Vec<u8>,
) -> Result<Arc<RecoveryMnemonic>, FfiError> {
    let secret = prf_secret(&prf_output)?;
    let inner = vault::open_entropy_under_secret(&secret, &sealed).map_err(|e| passkey_err(&e))?;
    Ok(Arc::new(RecoveryMnemonic::from_inner(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkey_vault_round_trips() {
        let m = Arc::new(RecoveryMnemonic::generate());
        let prf = vec![0x11u8; 32];
        let sealed = passkey_vault_seal(m.clone(), prf.clone()).unwrap();
        assert_eq!(sealed.len(), 44);
        let back = passkey_vault_open(prf, sealed).unwrap();
        assert_eq!(back.phrase(), m.phrase());
    }

    #[test]
    fn passkey_vault_refusals_are_typed() {
        let m = Arc::new(RecoveryMnemonic::generate());
        let sealed = passkey_vault_seal(m.clone(), vec![0x11u8; 32]).unwrap();
        assert!(matches!(
            passkey_vault_open(vec![0x12u8; 32], sealed.clone()),
            Err(FfiError::PasskeyVaultNotOpened)
        ));
        let mut tampered = sealed.clone();
        tampered[20] ^= 1;
        assert!(matches!(
            passkey_vault_open(vec![0x11u8; 32], tampered),
            Err(FfiError::PasskeyVaultNotOpened)
        ));
        match passkey_vault_open(vec![0x11u8; 31], sealed) {
            Err(FfiError::PasskeyVault(msg)) => assert!(msg.starts_with("bad-secret-length: ")),
            Err(other) => panic!("expected PasskeyVault, got {other:?}"),
            Ok(_) => panic!("a 31-byte PRF output must be refused"),
        }
        match passkey_vault_seal(m, vec![0u8; 33]) {
            Err(FfiError::PasskeyVault(msg)) => assert!(msg.starts_with("bad-secret-length: ")),
            other => panic!("expected PasskeyVault, got {other:?}"),
        }
    }
}
