// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! KeyPackage creation and wire-format helpers.
//!
//! A [`KeyPackage`] is the public artifact a device publishes so other users
//! can add it to MLS groups. The corresponding private encryption keypair is
//! stored inside the [`Identity`]'s provider; the resulting public package
//! can be transmitted freely.

use openmls::prelude::{KeyPackage, KeyPackageIn};
use tls_codec::{Deserialize as _, Serialize as _};

use crate::error::{Error, Result};
use crate::identity::Identity;

impl Identity {
    /// Create a fresh [`KeyPackage`] for this identity.
    ///
    /// The matching secret material is persisted in the identity's provider
    /// storage; calling this once produces one consumable KeyPackage. Servers
    /// typically request a batch and rotate them as they are consumed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyPackage`] on any OpenMLS-side failure.
    pub fn create_key_package(&self) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            // G-13 (ADR-0015): advertise the DeviceGroup removal-policy
            // extension type so this device can join a DeviceGroup that
            // carries the policy (OpenMLS validates every member's leaf
            // advertises a GroupContext extension type in use). Harmless for
            // non-DeviceGroup uses — they simply never carry the extension.
            .leaf_node_capabilities(crate::device_group::device_leaf_capabilities(
                self.ciphersuite,
            ))
            .build(
                self.ciphersuite,
                &self.provider,
                &self.signature_keys,
                self.credential.clone(),
            )
            .map_err(|e| Error::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }
}

/// Serialize a [`KeyPackage`] to its TLS-codec wire form.
///
/// # Errors
///
/// Returns [`Error::Codec`] on encoder failure (essentially impossible with
/// valid inputs, but the API surfaces it).
pub fn serialize(kp: &KeyPackage) -> Result<Vec<u8>> {
    kp.tls_serialize_detached()
        .map_err(|e| Error::Codec(format!("{e:?}")))
}

/// Decode and validate a wire-form KeyPackage.
///
/// Returns the validated [`KeyPackageIn`] which the caller can then standardly
/// validate against the active ciphersuite before use as an Add proposal.
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure.
pub fn deserialize(bytes: &[u8]) -> Result<KeyPackageIn> {
    let mut slice = bytes;
    KeyPackageIn::tls_deserialize(&mut slice).map_err(|e| Error::Codec(format!("{e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openmls_traits::OpenMlsProvider as _;

    #[test]
    fn create_and_roundtrip_key_package() {
        let alice = Identity::generate("alice").unwrap();
        let kp = alice.create_key_package().unwrap();

        let bytes = serialize(&kp).unwrap();
        assert!(!bytes.is_empty());

        let kp_in = deserialize(&bytes).unwrap();

        // Validate against ciphersuite — confirms the wire form is well-formed
        // and consistent with the suite the identity was generated under.
        kp_in
            .validate(
                alice.provider.crypto(),
                openmls::versions::ProtocolVersion::Mls10,
            )
            .expect("validate kpi");
    }

    #[test]
    fn each_call_produces_distinct_key_package() {
        let alice = Identity::generate("alice").unwrap();
        let kp1 = alice.create_key_package().unwrap();
        let kp2 = alice.create_key_package().unwrap();

        let b1 = serialize(&kp1).unwrap();
        let b2 = serialize(&kp2).unwrap();
        assert_ne!(b1, b2, "consecutive KeyPackages should be distinct");
    }
}
