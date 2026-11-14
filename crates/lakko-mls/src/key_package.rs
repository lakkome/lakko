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
#[cfg(test)]
use openmls::prelude::{Capabilities, ExtensionType, Lifetime};
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
            // ADR-0050 section 2.6 — a device's published KeyPackage is a
            // LAST-RESORT package (RFC 9420 section 10, `last_resort`
            // extension): OpenMLS keeps its private init key after a Welcome
            // built on it is opened, so ONE published package opens every
            // Welcome sealed to it. That is what admitting a device to N
            // groups in one pass needs (the pairing fan-out and a contact's
            // re-admission both fetch one package and propose N Adds), and
            // what the previous one-time posture broke: the second Welcome hit
            // a deleted key. The forward-secrecy cost — the init key survives
            // until the identity's storage is wiped — is recorded in the ADR
            // together with the follow-up (a batch of one-time packages beside
            // this one; pruning of superseded bundles). Clients still republish
            // after every consumed Welcome, so later adds use fresher keys.
            .mark_as_last_resort()
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
    /// **Adversary, for tests only.** Mint a KeyPackage whose leaf credential
    /// carries `credential_identity` verbatim — what a non-Lakko client that
    /// copied or corrupted a device binding would publish. Not part of the
    /// supported API; it exists so integration tests can prove every gate
    /// refuses such a leaf.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyPackage`] on any OpenMLS-side failure.
    #[doc(hidden)]
    pub fn adversary_key_package_with_credential(
        &self,
        credential_identity: Vec<u8>,
    ) -> Result<KeyPackage> {
        use openmls::credentials::{BasicCredential, CredentialWithKey};
        let credential = CredentialWithKey {
            credential: BasicCredential::new(credential_identity).into(),
            signature_key: self.signature_keys.public().into(),
        };
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(crate::device_group::lakko_leaf_capabilities(
                self.ciphersuite,
            ))
            .build(
                self.ciphersuite,
                &self.provider,
                &self.signature_keys,
                credential,
            )
            .map_err(|e| Error::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }

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

// ============================================================================
// The adversary half of the sweep filter's property tests.
//
// `Identity::create_key_package` always advertises the Lakko capability set, so
// nothing in this crate can mint the KeyPackage a hostile or simply
// differently-built client would publish. The `#[cfg(test)]` helpers below are
// that client: they call the SAME `KeyPackage::builder()` this crate uses and
// only weaken what the leaf advertises, which is exactly the freedom RFC 9420
// gives any conformant implementation.
//
// Nothing about such a KeyPackage is malformed — it verifies, it round-trips,
// and `KeyPackageIn::validate` accepts it, because "can this leaf live in THAT
// group" is a question only the group can ask. That is precisely why the answer
// has to be given at sweep time.
// ============================================================================

#[cfg(test)]
impl Identity {
    /// **Adversary.** Mint a KeyPackage whose leaf advertises support for no
    /// custom extension type at all — in particular not the ADR-0022 role table
    /// (`0xFF02`) every Lakko conversation group carries in its GroupContext.
    ///
    /// Adding it would make `CommitBuilder::build` fail with
    /// `ProposalValidationError::InsufficientCapabilities` on every committer.
    pub(crate) fn key_package_without_extension_capabilities(&self) -> Result<KeyPackage> {
        self.key_package_with_capabilities(Capabilities::new(
            None,
            Some(&[self.ciphersuite]),
            Some(&[]),
            None,
            None,
        ))
    }

    /// **Adversary.** Mint a KeyPackage whose leaf advertises the Lakko
    /// extension set but **not the group's ciphersuite**, which
    /// `validate_leaf_node_capabilities` refuses with
    /// `LeafNodeValidationError::CiphersuiteNotInCapabilities`.
    pub(crate) fn key_package_without_ciphersuite_capability(&self) -> Result<KeyPackage> {
        self.key_package_with_capabilities(Capabilities::new(
            None,
            // An empty ciphersuite list, so the group's own suite is absent.
            Some(&[]),
            Some(&[
                ExtensionType::Unknown(crate::device_group::DEVICE_POLICY_EXTENSION_TYPE),
                ExtensionType::Unknown(crate::group_roles::ROLE_EXTENSION_TYPE),
                ExtensionType::Unknown(crate::group_kind::GROUP_KIND_EXTENSION_TYPE),
            ]),
            None,
            None,
        ))
    }

    /// **Adversary.** Mint a KeyPackage whose leaf advertises no credential type
    /// at all, so it can satisfy neither its own credential
    /// (`validate_locally`) nor the existing members' (`valn0104`).
    pub(crate) fn key_package_without_credential_capability(&self) -> Result<KeyPackage> {
        self.key_package_with_capabilities(Capabilities::new(
            None,
            Some(&[self.ciphersuite]),
            Some(&[
                ExtensionType::Unknown(crate::device_group::DEVICE_POLICY_EXTENSION_TYPE),
                ExtensionType::Unknown(crate::group_roles::ROLE_EXTENSION_TYPE),
                ExtensionType::Unknown(crate::group_kind::GROUP_KIND_EXTENSION_TYPE),
            ]),
            None,
            Some(&[]),
        ))
    }

    /// **Not an adversary at all — just time.** Mint an otherwise ordinary
    /// KeyPackage whose lifetime has already run out.
    ///
    /// `Lifetime::new(0)` sets `not_after` to the current second, and
    /// `Lifetime::is_valid` requires `now < not_after` strictly, so this is
    /// expired the moment it is built. It stands in for the case that needs no
    /// hostility whatsoever: ADR-0045 gives a request no deadline, so a
    /// perfectly good KeyPackage can expire while its proposal waits.
    pub(crate) fn expired_key_package(&self) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(crate::device_group::lakko_leaf_capabilities(
                self.ciphersuite,
            ))
            .key_package_lifetime(Lifetime::new(0))
            .build(
                self.ciphersuite,
                &self.provider,
                &self.signature_keys,
                self.credential.clone(),
            )
            .map_err(|e| Error::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }

    /// **Time again.** An ordinary KeyPackage whose lifetime runs out
    /// `seconds` from now: admissible now, expired after a wait.
    #[cfg(test)]
    pub(crate) fn short_lived_key_package(&self, seconds: u64) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(crate::device_group::lakko_leaf_capabilities(
                self.ciphersuite,
            ))
            .key_package_lifetime(Lifetime::new(seconds))
            .build(
                self.ciphersuite,
                &self.provider,
                &self.signature_keys,
                self.credential.clone(),
            )
            .map_err(|e| Error::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }

    /// Shared tail: mint a KeyPackage with a caller-chosen capability set,
    /// otherwise identical to [`Identity::create_key_package`].
    fn key_package_with_capabilities(&self, capabilities: Capabilities) -> Result<KeyPackage> {
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(capabilities)
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

/// Decode a wire-form KeyPackage that must fill `bytes` exactly.
///
/// [`deserialize`] stops where the KeyPackage ends and ignores what follows,
/// which is right for a caller that frames its own input. A request to join
/// ([`crate::Group::retain_join_request`], ADR-0087 section 5) hands over the
/// KeyPackage alone, so anything after it, or an empty input, is refused.
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure, an empty input or trailing
/// bytes.
pub(crate) fn deserialize_exact(bytes: &[u8]) -> Result<KeyPackageIn> {
    if bytes.is_empty() {
        return Err(Error::Codec("empty KeyPackage".into()));
    }
    let mut slice = bytes;
    let kp_in =
        KeyPackageIn::tls_deserialize(&mut slice).map_err(|e| Error::Codec(format!("{e:?}")))?;
    if !slice.is_empty() {
        return Err(Error::Codec(format!(
            "{} bytes after the KeyPackage",
            slice.len()
        )));
    }
    Ok(kp_in)
}

/// Extract the leaf (Ed25519 signature) public key from a wire-form KeyPackage,
/// without validating it against the active ciphersuite.
///
/// The 32-byte signature key is the **stable per-identity leaf key**: distinct
/// KeyPackages minted by the same identity share it (only the HPKE init key is
/// fresh per KP). Used by the call-admission dedup (ADR-0032 / G-22) to reject a
/// peer re-minting fresh KeyPackages under one identity, where deduping on the
/// KeyPackage bytes alone would let each fresh KP through.
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure.
pub fn signature_key(bytes: &[u8]) -> Result<Vec<u8>> {
    let kp_in = deserialize(bytes)?;
    let cwk = kp_in.unverified_credential();
    Ok(cwk.signature_key.as_slice().to_vec())
}

/// Resolve a wire-form KeyPackage's leaf to the **account** key it belongs to
/// (ADR-0050): the device binding's account key when the leaf credential
/// carries a valid binding for this leaf, the leaf key itself for an unbound
/// Phase 1 leaf, and `None` for a leaf whose credential is neither — which every
/// admission gate refuses.
///
/// This is what the ADR-0032 call-admission dedup and a contact's device list
/// key on, since both must resolve a KeyPackage from a **non-member** before
/// any shared group exists.
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure.
pub fn account_key(bytes: &[u8]) -> Result<Option<[u8; 32]>> {
    let kp_in = deserialize(bytes)?;
    let cwk = kp_in.unverified_credential();
    Ok(crate::device_binding::account_key_of_credential(
        cwk.credential.serialized_content(),
        cwk.signature_key.as_slice(),
    ))
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
    fn signature_key_is_stable_across_fresh_key_packages() {
        let alice = Identity::generate("alice").unwrap();
        // Two FRESH KeyPackages from the same identity: different wire bytes
        // (fresh HPKE init key) but the SAME leaf signature key.
        let kp1 = serialize(&alice.create_key_package().unwrap()).unwrap();
        let kp2 = serialize(&alice.create_key_package().unwrap()).unwrap();
        assert_ne!(kp1, kp2, "fresh KeyPackages must differ on the wire");
        let sk1 = signature_key(&kp1).unwrap();
        let sk2 = signature_key(&kp2).unwrap();
        assert_eq!(sk1, sk2, "same identity → same leaf signature key");
        assert_eq!(sk1.len(), 32);
        // A different identity has a different signature key.
        let bob = Identity::generate("bob").unwrap();
        let bob_kp = serialize(&bob.create_key_package().unwrap()).unwrap();
        assert_ne!(signature_key(&bob_kp).unwrap(), sk1);
    }

    #[test]
    fn signature_key_rejects_garbage() {
        assert!(signature_key(&[0u8; 8]).is_err());
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
