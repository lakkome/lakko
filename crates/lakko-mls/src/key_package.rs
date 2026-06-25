// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! KeyPackage creation and wire-format helpers.
//!
//! A [`KeyPackage`] is the public artifact a device publishes so other users
//! can add it to MLS groups. The corresponding private encryption keypair is
//! stored inside the [`Identity`]'s provider; the resulting public package
//! can be transmitted freely.

use openmls::group::{GroupEpoch, GroupId};
use openmls::messages::external_proposals::JoinProposal;
use openmls::prelude::{KeyPackage, KeyPackageIn};
use openmls_rust_crypto::MemoryStorage;
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
            // Advertise support for all Lakko custom GroupContext extension
            // types (DeviceGroup removal-policy 0xFF01, ADR-0015; conversation
            // role table 0xFF02, ADR-0022) so this KeyPackage can be added to
            // either kind of group — OpenMLS validates every member's leaf
            // advertises each GroupContext extension type the group has in use.
            // Harmless for a group that uses neither.
            .leaf_node_capabilities(crate::device_group::lakko_leaf_capabilities(
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

    /// Create an external-join proposal — the joiner side of ADR-0027 (G-3).
    ///
    /// A **non-member** crafts an MLS external `Add` proposal
    /// ([`Sender::NewMemberProposal`](openmls::prelude::Sender), RFC 9420
    /// §12.1.8.3) for a group's current `group_id` + `epoch` (both learned
    /// out-of-band via a join ticket — the channel join-ticket of ADR-0010 is
    /// the planned carrier). The returned wire bytes are posted to the group's
    /// DS journal; an owner/admin surfaces the proposal via
    /// `Group::process_incoming_verifying_roles` (→
    /// `Incoming::ExternalJoinProposal`) and approves it with
    /// `Group::approve_external_join`, producing a Welcome the joiner opens with
    /// `Group::join_from_welcome`.
    ///
    /// The proposal embeds a fresh [`KeyPackage`] whose private init/encryption
    /// keys are persisted in **this** identity's provider, so the SAME identity
    /// must later open the resulting Welcome (the Welcome is HPKE-sealed to that
    /// KeyPackage's init key).
    ///
    /// # Errors
    ///
    /// [`Error::KeyPackage`] on KeyPackage generation failure, [`Error::Group`]
    /// on proposal construction failure, or [`Error::Codec`] on encode failure.
    pub fn create_external_join_proposal(&self, group_id: &[u8], epoch: u64) -> Result<Vec<u8>> {
        let kp = self.create_key_package()?;
        let msg = JoinProposal::new::<MemoryStorage>(
            kp,
            GroupId::from_slice(group_id),
            GroupEpoch::from(epoch),
            &self.signature_keys,
        )
        .map_err(|e| Error::Group(format!("external join proposal: {e:?}")))?;
        crate::group::encode_message(&msg)
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
