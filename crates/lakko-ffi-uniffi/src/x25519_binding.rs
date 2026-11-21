// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the account X25519 binding
//! (`lakko_contacts::x25519_binding`, security review 2026-11-21): the
//! identity key's signature over its X25519 key, sent with every identity
//! upload (`x25519_binding`, standard base64; discovery refuses an upload
//! without a valid one) and verified by a client before it seals anything to
//! a fetched `x25519_pubkey`. Mirror of
//! `crates/lakko-ffi-wasm/src/x25519_binding.rs`.

use lakko_contacts::x25519_binding::{
    sign_x25519_binding as rust_sign, verify_x25519_binding as rust_verify, X25519_BINDING_LEN,
};
use lakko_crypto::signing::SigningKey;

use crate::Identity;

/// The 64-byte binding of `account_identity`'s X25519 key to its Ed25519
/// identity key. Deterministic: every device of the account produces the
/// same bytes.
#[uniffi::export]
#[must_use]
pub fn sign_x25519_binding(account_identity: &Identity) -> Vec<u8> {
    let signing = SigningKey::from_seed(&account_identity.inner.ed25519_seed_bytes());
    rust_sign(&signing, &account_identity.inner.x25519_public_key()).to_vec()
}

/// `true` when `binding` is the signature of the identity key
/// `ed25519_public_key` over `x25519_public_key`. Anything else, inputs of
/// the wrong length or an absent binding included, is `false`: do not seal
/// to that X25519 key.
#[uniffi::export]
#[must_use]
pub fn verify_x25519_binding(
    binding: Vec<u8>,
    ed25519_public_key: Vec<u8>,
    x25519_public_key: Vec<u8>,
) -> bool {
    let (Ok(ed), Ok(x)) = (
        <[u8; 32]>::try_from(ed25519_public_key.as_slice()),
        <[u8; 32]>::try_from(x25519_public_key.as_slice()),
    ) else {
        return false;
    };
    rust_verify(&binding, &ed, &x).is_ok()
}

/// The binding's length (64).
#[uniffi::export]
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
        let id = Identity::from_seed(vec![0x31; 64]).unwrap();
        let binding = sign_x25519_binding(&id);
        assert_eq!(binding.len(), x25519_binding_len() as usize);
        let ed = id.inner.signature_public_key().to_vec();
        let x = id.inner.x25519_public_key().to_vec();
        assert!(verify_x25519_binding(
            binding.clone(),
            ed.clone(),
            x.clone()
        ));
        let other = Identity::from_seed(vec![0x32; 64]).unwrap();
        assert!(!verify_x25519_binding(
            binding.clone(),
            ed.clone(),
            other.inner.x25519_public_key().to_vec()
        ));
        assert!(!verify_x25519_binding(
            binding,
            other.inner.signature_public_key().to_vec(),
            x.clone()
        ));
        assert!(!verify_x25519_binding(vec![], ed, x));
    }
}
