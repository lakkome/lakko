// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm surface for the account X25519 binding
//! (`lakko_contacts::x25519_binding`, security review 2026-11-21): the
//! identity key's signature over its X25519 key, sent with every identity
//! upload (`x25519_binding`, standard base64; discovery refuses an upload
//! without a valid one) and verified by a client before it seals anything to
//! a fetched `x25519_pubkey`. Mirror of
//! `crates/lakko-ffi-uniffi/src/x25519_binding.rs`.

use lakko_contacts::x25519_binding::{
    sign_x25519_binding as rust_sign, verify_x25519_binding as rust_verify, X25519_BINDING_LEN,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

/// The 64-byte binding of `accountIdentity`'s X25519 key to its Ed25519
/// identity key. Deterministic: every device of the account produces the
/// same bytes.
#[wasm_bindgen(js_name = signX25519Binding)]
#[must_use]
pub fn sign_x25519_binding(account_identity: &Identity) -> Vec<u8> {
    let signing = SigningKey::from_seed(&account_identity.mls_identity().ed25519_seed_bytes());
    rust_sign(
        &signing,
        &account_identity.mls_identity().x25519_public_key(),
    )
    .to_vec()
}

/// `true` when `binding` is the signature of the identity key
/// `ed25519PublicKey` over `x25519PublicKey`. Anything else, inputs of the
/// wrong length or an absent binding included, is `false`: do not seal to
/// that X25519 key.
#[wasm_bindgen(js_name = verifyX25519Binding)]
#[must_use]
pub fn verify_x25519_binding(
    binding: &[u8],
    ed25519_public_key: &[u8],
    x25519_public_key: &[u8],
) -> bool {
    let (Ok(ed), Ok(x)) = (
        <[u8; 32]>::try_from(ed25519_public_key),
        <[u8; 32]>::try_from(x25519_public_key),
    ) else {
        return false;
    };
    rust_verify(binding, &ed, &x).is_ok()
}

/// The binding's length (64).
#[wasm_bindgen(js_name = x25519BindingLen)]
#[must_use]
pub fn x25519_binding_len() -> u32 {
    u32::try_from(X25519_BINDING_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_binding_verifies_for_its_identity_only() {
        let id = lakko_mls::Identity::from_seed(&[0x31; 64]).unwrap();
        let signing = SigningKey::from_seed(&id.ed25519_seed_bytes());
        let binding = rust_sign(&signing, &id.x25519_public_key()).to_vec();
        assert_eq!(binding.len(), x25519_binding_len() as usize);
        let ed = id.signature_public_key().to_vec();
        let x = id.x25519_public_key().to_vec();
        assert!(verify_x25519_binding(&binding, &ed, &x));
        assert!(!verify_x25519_binding(&binding, &ed, &[0x44; 32]));
        assert!(!verify_x25519_binding(&[], &ed, &x));
    }
}
