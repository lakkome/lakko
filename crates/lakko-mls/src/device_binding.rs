// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device binding — the signed statement that a per-device leaf key belongs to
//! an account ([ADR-0050]).
//!
//! Since ADR-0050 every conversation leaf is the per-device `DeviceIdentity`,
//! never the account key. What ties a leaf back to the person is this blob,
//! signed by the **account** key and carried in the leaf's `BasicCredential`
//! identity bytes, which OpenMLS signs as part of the leaf, carries verbatim,
//! and never interprets. Every peer verifies it in Lakko code at every gate
//! that admits or authorises a leaf; OpenMLS checks nothing in it.
//!
//! ```text
//! version(1)=0x01 ‖ account_pubkey(32) ‖ device_pubkey(32) ‖ issued_at_unix_secs(8 BE) ‖ sig(64)
//! ```
//!
//! `sig` is Ed25519 by the account key over
//! `DOMAIN ‖ version ‖ account_pubkey ‖ device_pubkey ‖ issued_at`.
//!
//! The binding proves "this device key was authorised by this account key". It
//! does **not** prove the account key belongs to anyone the receiver knows;
//! that is the receiver's contact pin (ADR-0016), applied to
//! [`DeviceBinding::account_pubkey`] exactly as to a Phase 1 leaf.
//!
//! # The resolver
//!
//! [`account_key_of_credential`] is the one place the two leaf generations
//! meet. A credential carrying a valid binding resolves to the binding's
//! account key. An **empty** credential — every leaf minted before ADR-0050 —
//! resolves to the leaf's own signature key, because a Phase 1 leaf *is* the
//! account key. Anything else (a malformed or forged binding, a binding whose
//! `device_pubkey` is not the leaf's key) resolves to nothing, and every gate
//! treats "nothing" as refused (fail-closed, invariant #4).
//!
//! [ADR-0050]: ../../../docs/adr/0050-per-device-conversation-leaves.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};

use crate::error::{Error, Result};

/// Domain separator for the binding signature. Distinct from every other
/// Lakko Ed25519 domain (`lakko/device-attest/v1`, `lakko/genesis-attest/v1`,
/// the supersession and leaf-genesis domains), so a signature minted for one
/// purpose can never verify as a binding.
const DOMAIN: &[u8] = b"lakko/device-binding/v1";

/// Current wire version. An unknown version is refused.
const VERSION: u8 = 1;

/// Ed25519 public key length.
const KEY_LEN: usize = 32;

/// Big-endian unix-seconds timestamp length.
const ISSUED_AT_LEN: usize = 8;

/// Ed25519 signature length.
const SIGNATURE_LEN: usize = 64;

/// Total wire length: `1 + 32 + 32 + 8 + 64`.
pub const DEVICE_BINDING_LEN: usize = 1 + KEY_LEN + KEY_LEN + ISSUED_AT_LEN + SIGNATURE_LEN;

/// Length of the signed prefix (everything but the signature).
const SIGNED_PREFIX_LEN: usize = DEVICE_BINDING_LEN - SIGNATURE_LEN;

/// The authenticated claims of a device binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceBinding {
    /// The account's Ed25519 public key — the identity contacts pin and the
    /// KT log binds.
    pub account_pubkey: [u8; KEY_LEN],
    /// The device leaf's Ed25519 public key. Must equal the signature key of
    /// the leaf that carries the binding.
    pub device_pubkey: [u8; KEY_LEN],
    /// When the account key signed this device in, unix seconds. Informational
    /// (shown on the Devices screen); this version sets no expiry.
    pub issued_at_unix_secs: u64,
}

impl DeviceBinding {
    /// The exact bytes the signature covers.
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(DOMAIN.len() + SIGNED_PREFIX_LEN);
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.account_pubkey);
        buf.extend_from_slice(&self.device_pubkey);
        buf.extend_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        buf
    }

    /// Mint a binding for `device_pubkey` under the account whose Ed25519 seed
    /// is `account_seed`, and return its wire form.
    #[must_use]
    pub fn sign(
        account_seed: &[u8; KEY_LEN],
        device_pubkey: [u8; KEY_LEN],
        issued_at_unix_secs: u64,
    ) -> [u8; DEVICE_BINDING_LEN] {
        let signing = SigningKey::from_seed(account_seed);
        let claims = Self {
            account_pubkey: signing.verifying_key().to_bytes(),
            device_pubkey,
            issued_at_unix_secs,
        };
        let sig = signing.sign(&claims.signing_input());
        let mut out = [0u8; DEVICE_BINDING_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + KEY_LEN].copy_from_slice(&claims.account_pubkey);
        off += KEY_LEN;
        out[off..off + KEY_LEN].copy_from_slice(&claims.device_pubkey);
        off += KEY_LEN;
        out[off..off + ISSUED_AT_LEN].copy_from_slice(&claims.issued_at_unix_secs.to_be_bytes());
        off += ISSUED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the signature against the **embedded**
    /// account key. Bounded and fail-closed: a wrong length, an unknown
    /// version, a malformed key or signature, or a signature that does not
    /// verify is an [`Error::Identity`].
    ///
    /// This does not check that `device_pubkey` matches any particular leaf;
    /// [`Self::verify_for_leaf`] does.
    ///
    /// # Errors
    ///
    /// [`Error::Identity`] as described above.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != DEVICE_BINDING_LEN {
            return Err(Error::Identity(format!(
                "device binding: expected {DEVICE_BINDING_LEN} bytes, got {}",
                bytes.len()
            )));
        }
        if bytes[0] != VERSION {
            return Err(Error::Identity(format!(
                "device binding: unknown version {}",
                bytes[0]
            )));
        }
        let mut off = 1;
        let mut account_pubkey = [0u8; KEY_LEN];
        account_pubkey.copy_from_slice(&bytes[off..off + KEY_LEN]);
        off += KEY_LEN;
        let mut device_pubkey = [0u8; KEY_LEN];
        device_pubkey.copy_from_slice(&bytes[off..off + KEY_LEN]);
        off += KEY_LEN;
        let mut issued = [0u8; ISSUED_AT_LEN];
        issued.copy_from_slice(&bytes[off..off + ISSUED_AT_LEN]);
        off += ISSUED_AT_LEN;
        let claims = Self {
            account_pubkey,
            device_pubkey,
            issued_at_unix_secs: u64::from_be_bytes(issued),
        };
        let verifying = VerifyingKey::from_bytes(&account_pubkey)
            .map_err(|e| Error::Identity(format!("device binding: account key: {e}")))?;
        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|e| Error::Identity(format!("device binding: signature: {e}")))?;
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| Error::Identity("device binding: signature does not verify".into()))?;
        Ok(claims)
    }

    /// Decode + verify, and additionally require that the binding names
    /// `leaf_signature_key` as its device — the check every gate runs against
    /// the leaf that carries the binding.
    ///
    /// # Errors
    ///
    /// [`Error::Identity`] on any decode failure or a device-key mismatch.
    pub fn verify_for_leaf(bytes: &[u8], leaf_signature_key: &[u8]) -> Result<Self> {
        let claims = Self::decode(bytes)?;
        if claims.device_pubkey.as_slice() != leaf_signature_key {
            return Err(Error::Identity(
                "device binding: device key does not match the leaf's signature key".into(),
            ));
        }
        Ok(claims)
    }
}

/// Resolve a leaf's credential identity bytes + signature key to the account
/// key the leaf belongs to.
///
/// * a valid binding for this leaf → the binding's account key;
/// * an **empty** credential → the leaf's own signature key (a Phase 1 leaf,
///   whose key is the account key);
/// * anything else → `None`, which every gate reads as refused.
#[must_use]
pub fn account_key_of_credential(
    credential_identity: &[u8],
    leaf_signature_key: &[u8],
) -> Option<[u8; KEY_LEN]> {
    if credential_identity.is_empty() {
        return leaf_signature_key.try_into().ok();
    }
    DeviceBinding::verify_for_leaf(credential_identity, leaf_signature_key)
        .ok()
        .map(|b| b.account_pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn device_key(b: u8) -> [u8; 32] {
        SigningKey::from_seed(&seed(b)).verifying_key().to_bytes()
    }

    #[test]
    fn sign_then_decode_round_trips() {
        let account = seed(1);
        let device = device_key(2);
        let bytes = DeviceBinding::sign(&account, device, 1_700_000_000);
        assert_eq!(bytes.len(), DEVICE_BINDING_LEN);
        let claims = DeviceBinding::decode(&bytes).expect("decode");
        assert_eq!(
            claims.account_pubkey,
            SigningKey::from_seed(&account).verifying_key().to_bytes()
        );
        assert_eq!(claims.device_pubkey, device);
        assert_eq!(claims.issued_at_unix_secs, 1_700_000_000);
        assert_eq!(
            account_key_of_credential(&bytes, &device),
            Some(claims.account_pubkey)
        );
    }

    #[test]
    fn a_binding_for_another_leaf_does_not_resolve() {
        let bytes = DeviceBinding::sign(&seed(1), device_key(2), 0);
        assert!(DeviceBinding::verify_for_leaf(&bytes, &device_key(3)).is_err());
        assert_eq!(account_key_of_credential(&bytes, &device_key(3)), None);
    }

    #[test]
    fn a_tampered_binding_is_refused() {
        let mut bytes = DeviceBinding::sign(&seed(1), device_key(2), 42);
        // Flip one bit of the account key: the signature no longer verifies
        // under the embedded key.
        bytes[5] ^= 0x01;
        assert!(DeviceBinding::decode(&bytes).is_err());
        let mut bytes = DeviceBinding::sign(&seed(1), device_key(2), 42);
        // Flip one bit of the timestamp: covered by the signature.
        bytes[1 + 64 + 7] ^= 0x01;
        assert!(DeviceBinding::decode(&bytes).is_err());
    }

    #[test]
    fn wrong_length_and_version_are_refused() {
        let bytes = DeviceBinding::sign(&seed(1), device_key(2), 0);
        assert!(DeviceBinding::decode(&bytes[..DEVICE_BINDING_LEN - 1]).is_err());
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert!(DeviceBinding::decode(&longer).is_err());
        let mut versioned = bytes;
        versioned[0] = 2;
        assert!(DeviceBinding::decode(&versioned).is_err());
        assert!(DeviceBinding::decode(&[]).is_err());
    }

    #[test]
    fn an_empty_credential_resolves_to_the_leaf_key_and_garbage_to_nothing() {
        let leaf = device_key(7);
        assert_eq!(account_key_of_credential(&[], &leaf), Some(leaf));
        assert_eq!(account_key_of_credential(b"alice", &leaf), None);
        assert_eq!(account_key_of_credential(&[], &leaf[..31]), None);
    }

    #[test]
    fn the_domain_keeps_an_attestation_from_passing_as_a_binding() {
        // Sign the same field layout under a different domain and check the
        // binding decoder refuses it: the domain is part of the signed input.
        let account = seed(1);
        let signing = SigningKey::from_seed(&account);
        let claims = DeviceBinding {
            account_pubkey: signing.verifying_key().to_bytes(),
            device_pubkey: device_key(2),
            issued_at_unix_secs: 9,
        };
        let mut other_input = b"lakko/device-attest/v1".to_vec();
        other_input.extend_from_slice(&claims.signing_input()[DOMAIN.len()..]);
        let sig = signing.sign(&other_input);
        let mut bytes = DeviceBinding::sign(&account, claims.device_pubkey, 9);
        bytes[SIGNED_PREFIX_LEN..].copy_from_slice(&sig.to_bytes());
        assert!(DeviceBinding::decode(&bytes).is_err());
    }
}
