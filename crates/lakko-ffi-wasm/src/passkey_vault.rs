// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko_mnemonic::vault` (ADR-0072 decision 6):
//! the passkey vault.
//!
//! `passkeyVaultSeal` / `passkeyVaultOpen` seal and open the phrase entropy
//! under a passkey's 32-byte PRF output. The PRF output never leaves the
//! device; the server stores the blob opaque.

use lakko_mnemonic::vault;
use wasm_bindgen::prelude::*;

use crate::mnemonic::RecoveryMnemonic;

fn vault_err(e: vault::VaultError) -> JsError {
    JsError::new(&e.to_string())
}

/// Seal a mnemonic's entropy under a passkey's 32-byte PRF output
/// (ADR-0072 decision 6). The result is the opaque blob `PUT /vault/passkey`
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
    fn passkey_vault_round_trip_over_the_wasm_shape() {
        let m = RecoveryMnemonic::generate();
        let prf = [0x11u8; 32];
        let sealed = passkey_vault_seal(&prf, &m).unwrap();
        assert_eq!(sealed.len(), 44);
        let back = passkey_vault_open(&prf, &sealed).unwrap();
        assert_eq!(back.words(), m.words());
        // The refusal paths (wrong PRF output, short input) construct a
        // `JsError`, which only exists on wasm32; they are covered by the
        // `lakko-mnemonic` vault tests.
    }
}
