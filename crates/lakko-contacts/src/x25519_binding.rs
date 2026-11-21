// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's X25519 key, signed by its Ed25519 identity key (security
//! review 2026-11-21).
//!
//! ## Why
//!
//! An account identity carries two key pairs derived from separate seeds
//! (`lakko_mls::Identity::from_seed`): the Ed25519 identity key, which the
//! directory, the KT log and every pin check, and an X25519 key that every
//! sealed-to-account envelope is sealed to (registrar seeds and the root
//! handover, knock seeds, admission seeds, channel read keys, channel epoch
//! seeds, group contact requests, contact requests). The identity bundle on
//! the directory serves both, but nothing tied the second to the first: a
//! client that checked only the Ed25519 key would seal those secrets to
//! whatever X25519 key a hostile directory served beside it.
//!
//! Now the identity key signs its X25519 key, the directory refuses an
//! identity upload without a valid signature, and a client verifies it
//! ([`verify_x25519_binding`]) against the Ed25519 key it trusts before it
//! seals anything to the X25519 key. Both keys are public, so the directory
//! can verify too (invariant #2 holds: it stores a public artefact).
//!
//! ## Wire format
//!
//! ```text
//! binding (64) = Ed25519_identity(X25519_BINDING_DOMAIN ‖ ed25519_pub(32) ‖ x25519_pub(32))
//! ```
//!
//! The signature alone: both keys travel beside it in the identity bundle.
//! The Ed25519 public key is in the signed bytes so a signature never
//! verifies under another key's name.

use core::result::Result as CoreResult;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

/// Domain separator the identity key signs its X25519 key under. Frozen.
pub const X25519_BINDING_DOMAIN: &[u8] = b"lakko/account/x25519-binding/v1\x00";

/// Length of each public key.
pub const X25519_BINDING_KEY_LEN: usize = 32;

/// Wire length of a binding: one Ed25519 signature.
pub const X25519_BINDING_LEN: usize = 64;

const SIGNED_LEN: usize = X25519_BINDING_DOMAIN.len() + 2 * X25519_BINDING_KEY_LEN;

/// Why a binding was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum X25519BindingError {
    /// The binding is not [`X25519_BINDING_LEN`] bytes.
    #[error("x25519 binding has the wrong length")]
    BadLength,
    /// The Ed25519 key is not a usable public key (malformed or weak), or
    /// the X25519 key is all zeros.
    #[error("x25519 binding names an unusable key")]
    BadKey,
    /// The signature does not verify.
    #[error("x25519 binding does not verify")]
    BadSignature,
}

impl X25519BindingError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::BadKey => "badKey",
            Self::BadSignature => "badSignature",
        }
    }
}

fn signed_bytes(
    ed25519_pub: &[u8; X25519_BINDING_KEY_LEN],
    x25519_pub: &[u8; X25519_BINDING_KEY_LEN],
) -> [u8; SIGNED_LEN] {
    let mut out = [0u8; SIGNED_LEN];
    let d = X25519_BINDING_DOMAIN.len();
    out[..d].copy_from_slice(X25519_BINDING_DOMAIN);
    out[d..d + X25519_BINDING_KEY_LEN].copy_from_slice(ed25519_pub);
    out[d + X25519_BINDING_KEY_LEN..].copy_from_slice(x25519_pub);
    out
}

/// Sign `x25519_pub` with the account identity key. Deterministic (Ed25519
/// signatures are), so every device of the account produces the same bytes.
#[must_use]
pub fn sign_x25519_binding(
    identity: &SigningKey,
    x25519_pub: &[u8; X25519_BINDING_KEY_LEN],
) -> [u8; X25519_BINDING_LEN] {
    let ed25519_pub = identity.verifying_key().to_bytes();
    identity
        .sign(&signed_bytes(&ed25519_pub, x25519_pub))
        .to_bytes()
}

/// Verify that the identity key `ed25519_pub` signed `x25519_pub`.
///
/// # Errors
///
/// [`X25519BindingError::BadLength`], [`X25519BindingError::BadKey`],
/// [`X25519BindingError::BadSignature`].
pub fn verify_x25519_binding(
    binding: &[u8],
    ed25519_pub: &[u8; X25519_BINDING_KEY_LEN],
    x25519_pub: &[u8; X25519_BINDING_KEY_LEN],
) -> CoreResult<(), X25519BindingError> {
    if binding.len() != X25519_BINDING_LEN {
        return Err(X25519BindingError::BadLength);
    }
    let key = VerifyingKey::from_bytes(ed25519_pub).map_err(|_| X25519BindingError::BadKey)?;
    if key.is_weak() || x25519_pub.iter().all(|b| *b == 0) {
        return Err(X25519BindingError::BadKey);
    }
    let signature = Signature::from_bytes(binding).map_err(|_| X25519BindingError::BadSignature)?;
    key.verify(&signed_bytes(ed25519_pub, x25519_pub), &signature)
        .map_err(|_| X25519BindingError::BadSignature)
}

/// The fuzz oracle behind `fuzz_api::x25519_binding_verify`: verify `bytes`
/// (split as binding, Ed25519 key, X25519 key) and require that nothing
/// verifies under a fixed key the fuzzer does not hold, and that a binding
/// this module signs verifies. `true` when both hold.
pub(crate) fn fuzz_verify(bytes: &[u8]) -> bool {
    let fixed = SigningKey::from_seed(&[0x5C; 32]);
    let fixed_pub = fixed.verifying_key().to_bytes();
    let mut x = [0u8; X25519_BINDING_KEY_LEN];
    let take = bytes.len().min(X25519_BINDING_KEY_LEN);
    x[..take].copy_from_slice(&bytes[..take]);
    if bytes.len() >= X25519_BINDING_LEN + 2 * X25519_BINDING_KEY_LEN {
        let (sig, rest) = bytes.split_at(X25519_BINDING_LEN);
        let mut ed = [0u8; 32];
        ed.copy_from_slice(&rest[..32]);
        let mut xx = [0u8; 32];
        xx.copy_from_slice(&rest[32..64]);
        let _ = verify_x25519_binding(sig, &ed, &xx);
    }
    if verify_x25519_binding(bytes, &fixed_pub, &x).is_ok() {
        return false;
    }
    if x.iter().all(|b| *b == 0) {
        return true;
    }
    verify_x25519_binding(&sign_x25519_binding(&fixed, &x), &fixed_pub, &x).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn hex(b: &[u8]) -> String {
        use core::fmt::Write as _;
        b.iter().fold(String::new(), |mut out, x| {
            let _ = write!(out, "{x:02x}");
            out
        })
    }

    #[test]
    fn a_signed_key_verifies_and_nothing_else_does() {
        let id = SigningKey::from_seed(&[0x11; 32]);
        let id_pub = id.verifying_key().to_bytes();
        let x = [0x33; 32];
        let binding = sign_x25519_binding(&id, &x);
        assert_eq!(verify_x25519_binding(&binding, &id_pub, &x), Ok(()));
        // Another X25519 key under the same signature.
        assert_eq!(
            verify_x25519_binding(&binding, &id_pub, &[0x34; 32]),
            Err(X25519BindingError::BadSignature)
        );
        // Another identity key.
        let other = SigningKey::from_seed(&[0x12; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_x25519_binding(&binding, &other, &x),
            Err(X25519BindingError::BadSignature)
        );
        // Wrong lengths and unusable keys.
        assert_eq!(
            verify_x25519_binding(&binding[..63], &id_pub, &x),
            Err(X25519BindingError::BadLength)
        );
        assert_eq!(
            verify_x25519_binding(&binding, &id_pub, &[0u8; 32]),
            Err(X25519BindingError::BadKey)
        );
        let mut identity_point = [0u8; 32];
        identity_point[0] = 1;
        assert_eq!(
            verify_x25519_binding(&binding, &identity_point, &x),
            Err(X25519BindingError::BadKey)
        );
    }

    /// Another use's signature over the same keys (a mailbox binding, a
    /// plain signature over the raw keys) does not verify as this binding.
    #[test]
    fn a_signature_made_for_another_purpose_does_not_verify() {
        let id = SigningKey::from_seed(&[0x21; 32]);
        let id_pub = id.verifying_key().to_bytes();
        let x = [0x44; 32];
        let mut raw = Vec::new();
        raw.extend_from_slice(&id_pub);
        raw.extend_from_slice(&x);
        let plain = id.sign(&raw).to_bytes();
        assert_eq!(
            verify_x25519_binding(&plain, &id_pub, &x),
            Err(X25519BindingError::BadSignature)
        );
        let mailbox = crate::mailbox::MailboxBinding::sign(&id, &x).encode();
        assert_eq!(
            verify_x25519_binding(&mailbox[1..65], &id_pub, &x),
            Err(X25519BindingError::BadSignature)
        );
    }

    /// Golden: pins the domain and the layout, for the clients and the
    /// directory.
    #[test]
    fn golden_binding() {
        let id = SigningKey::from_seed(&[0x42; 32]);
        let binding = sign_x25519_binding(&id, &[0x24; 32]);
        assert_eq!(
            hex(&binding),
            "390fd885133d851f2a5f13dda547078504fb8000c4416540b34a27257df09b9023f0e7eeec8e25af90bb01b7e8670af49acb7f41864148f2173db086223aa600"
        );
    }

    proptest! {
        #[test]
        fn verify_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            prop_assert!(fuzz_verify(&bytes));
        }

        #[test]
        fn any_signed_key_verifies(seed in any::<[u8; 32]>(), x in any::<[u8; 32]>()) {
            prop_assume!(x.iter().any(|b| *b != 0));
            let id = SigningKey::from_seed(&seed);
            let pk = id.verifying_key().to_bytes();
            prop_assert_eq!(verify_x25519_binding(&sign_x25519_binding(&id, &x), &pk, &x), Ok(()));
        }
    }
}
