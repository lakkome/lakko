// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The DeviceGroup self-removal proof ([ADR-0081] amendment, closing G-52).
//!
//! Under the ADR-0015 attested-removal policy a DeviceGroup commit that
//! removes a leaf is refused by every peer unless an attestation from the
//! auth service (a passkey gesture) covers the removed leaf. The policy
//! exists so a stolen session or on-device malware cannot remove ANOTHER
//! device. A device that lost its DeviceGroup state and rejoins (ADR-0081
//! decision 4) asks for its own previous leaf to be replaced by a new one;
//! a leaf asking for its own removal is never the threat the policy guards
//! against, so the gate also accepts a removal covered by a proof signed
//! with the removed leaf's own signature key.
//!
//! **What the proof binds.** An Ed25519 signature by the removed leaf's
//! signature key over
//!
//! ```text
//! "lakko/devicegroup/self-removal/v1\0"
//!   ‖ group_id_len (1) ‖ group_id
//!   ‖ epoch (8, big-endian)
//!   ‖ removed_signature_pubkey (32)
//!   ‖ replacement_key_package_ref (32)
//! ```
//!
//! where `epoch` is the DeviceGroup epoch the replacing commit must leave
//! (the journal's latest epoch when the device knocked) and the last field is
//! the RFC 9420 `KeyPackageRef` of the KeyPackage that replaces the leaf. The
//! gate accepts the proof for a removed leaf only when the commit leaves that
//! epoch AND adds that KeyPackage. So the proof cannot be replayed: not with
//! another KeyPackage (another reference), and not with the same one later
//! (the replacement leaf keeps the removed leaf's signature key, so without
//! the epoch any member could later remove the rejoined device and re-add
//! the stale KeyPackage, a removal the policy forbids; one epoch has one
//! commit). It cannot remove another leaf (its key is in the statement) nor
//! act in another account's DeviceGroup (the group id is in the statement).
//!
//! **Wire.** Fixed length, [`SELF_REMOVAL_PROOF_LEN`] = 137 bytes:
//! `version (0x01) ‖ epoch (8, big-endian) ‖ removed_signature_pubkey (32) ‖
//! key_package_ref (32) ‖ signature (64)`. The group id is not carried: the
//! verifier knows which group it is applying a commit to. The decoder is
//! bounded and fail-closed: any other length or version is an error, never a
//! panic. The length differs from an ADR-0015 action attestation (121
//! bytes), so both travel in the same removal-authorisation slot of the
//! DeviceGroup journal envelope and the gate tries each entry as either.
//!
//! [ADR-0081]: ../../../docs/adr/0081-devicegroup-commits-cannot-strand.md

use lakko_crypto::signing::{Signature, VerifyingKey};
use openmls_traits::signatures::Signer as _;
use openmls_traits::OpenMlsProvider as _;

use crate::error::{Error, Result};
use crate::identity::Identity;

/// Domain separator of the signed statement. Frozen.
pub const SELF_REMOVAL_DOMAIN: &[u8] = b"lakko/devicegroup/self-removal/v1\0";

/// The only wire version.
pub const SELF_REMOVAL_VERSION: u8 = 0x01;

/// Length of the epoch, of an Ed25519 public key, of a `KeyPackageRef`
/// under Lakko's ciphersuite (SHA-256), and of an Ed25519 signature.
const EPOCH_LEN: usize = 8;
const KEY_LEN: usize = 32;
const REF_LEN: usize = 32;
const SIG_LEN: usize = 64;

/// The wire length of a proof.
pub const SELF_REMOVAL_PROOF_LEN: usize = 1 + EPOCH_LEN + KEY_LEN + REF_LEN + SIG_LEN;

/// A decoded (not yet verified) self-removal proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfRemovalProof {
    /// The DeviceGroup epoch the replacing commit must leave.
    pub epoch: u64,
    /// The signature key of the leaf whose removal it asks for.
    pub removed_signature_pubkey: [u8; KEY_LEN],
    /// The `KeyPackageRef` of the KeyPackage that replaces it.
    pub key_package_ref: [u8; REF_LEN],
    /// The Ed25519 signature over [`statement`].
    pub signature: [u8; SIG_LEN],
}

/// The signed statement for `group_id`, `epoch`, `removed_signature_pubkey`
/// and `key_package_ref`.
///
/// # Errors
///
/// [`Error::Group`] when `group_id` is empty or longer than 255 bytes.
pub fn statement(
    group_id: &[u8],
    epoch: u64,
    removed_signature_pubkey: &[u8; KEY_LEN],
    key_package_ref: &[u8; REF_LEN],
) -> Result<Vec<u8>> {
    let len = u8::try_from(group_id.len())
        .ok()
        .filter(|l| *l > 0)
        .ok_or_else(|| Error::Group("self-removal proof: group id must be 1..=255 bytes".into()))?;
    let mut out = Vec::with_capacity(
        SELF_REMOVAL_DOMAIN.len() + 1 + group_id.len() + EPOCH_LEN + KEY_LEN + REF_LEN,
    );
    out.extend_from_slice(SELF_REMOVAL_DOMAIN);
    out.push(len);
    out.extend_from_slice(group_id);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(removed_signature_pubkey);
    out.extend_from_slice(key_package_ref);
    Ok(out)
}

impl SelfRemovalProof {
    /// Encode to the fixed wire layout.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SELF_REMOVAL_PROOF_LEN);
        out.push(SELF_REMOVAL_VERSION);
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.removed_signature_pubkey);
        out.extend_from_slice(&self.key_package_ref);
        out.extend_from_slice(&self.signature);
        out
    }

    /// Decode the fixed wire layout. Bounded and fail-closed.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] for any length other than
    /// [`SELF_REMOVAL_PROOF_LEN`] or any version other than
    /// [`SELF_REMOVAL_VERSION`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != SELF_REMOVAL_PROOF_LEN {
            return Err(Error::Codec(format!(
                "self-removal proof: length {} (want {SELF_REMOVAL_PROOF_LEN})",
                bytes.len()
            )));
        }
        if bytes[0] != SELF_REMOVAL_VERSION {
            return Err(Error::Codec(format!(
                "self-removal proof: unknown version {}",
                bytes[0]
            )));
        }
        let mut epoch = [0u8; EPOCH_LEN];
        let mut removed_signature_pubkey = [0u8; KEY_LEN];
        let mut key_package_ref = [0u8; REF_LEN];
        let mut signature = [0u8; SIG_LEN];
        let key_at = 1 + EPOCH_LEN;
        let ref_at = key_at + KEY_LEN;
        let sig_at = ref_at + REF_LEN;
        epoch.copy_from_slice(&bytes[1..key_at]);
        removed_signature_pubkey.copy_from_slice(&bytes[key_at..ref_at]);
        key_package_ref.copy_from_slice(&bytes[ref_at..sig_at]);
        signature.copy_from_slice(&bytes[sig_at..]);
        Ok(Self {
            epoch: u64::from_be_bytes(epoch),
            removed_signature_pubkey,
            key_package_ref,
            signature,
        })
    }

    /// `true` when this proof, in `group_id`, authorises removing the leaf
    /// with `removed_signature_pubkey` in a commit that leaves `epoch` and
    /// whose added KeyPackages have the references `added_key_package_refs`:
    /// the epoch and the key named are the commit's and the removed leaf's,
    /// the KeyPackage named is added by the same commit, and the signature
    /// verifies under the removed leaf's key. Never panics.
    #[must_use]
    pub fn covers(
        &self,
        group_id: &[u8],
        epoch: u64,
        removed_signature_pubkey: &[u8],
        added_key_package_refs: &[Vec<u8>],
    ) -> bool {
        if self.epoch != epoch {
            return false;
        }
        if self.removed_signature_pubkey.as_slice() != removed_signature_pubkey {
            return false;
        }
        if !added_key_package_refs
            .iter()
            .any(|r| r.as_slice() == self.key_package_ref.as_slice())
        {
            return false;
        }
        let Ok(message) = statement(
            group_id,
            self.epoch,
            &self.removed_signature_pubkey,
            &self.key_package_ref,
        ) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&self.removed_signature_pubkey) else {
            return false;
        };
        let Ok(signature) = Signature::from_bytes(&self.signature) else {
            return false;
        };
        key.verify(&message, &signature).is_ok()
    }
}

/// The RFC 9420 `KeyPackageRef` of a wire-format KeyPackage, computed with
/// `identity`'s crypto provider after the package validates.
///
/// # Errors
///
/// [`Error::KeyPackage`] when the package does not parse or validate, or its
/// reference is not 32 bytes.
pub fn key_package_ref(identity: &Identity, key_package_wire: &[u8]) -> Result<[u8; REF_LEN]> {
    let kp_in = crate::key_package::deserialize(key_package_wire)?;
    let kp = kp_in
        .validate(
            identity.provider.crypto(),
            openmls::prelude::ProtocolVersion::Mls10,
        )
        .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))?;
    let reference = kp
        .hash_ref(identity.provider.crypto())
        .map_err(|e| Error::KeyPackage(format!("hash_ref: {e:?}")))?;
    <[u8; REF_LEN]>::try_from(reference.as_slice())
        .map_err(|_| Error::KeyPackage("key package reference is not 32 bytes".into()))
}

/// Sign a self-removal proof with `old_identity`'s leaf signature key: the
/// device asks, in `group_id`, that its leaf be replaced by the KeyPackage
/// `replacement_key_package_wire` in the commit that leaves `epoch` (the
/// journal's latest epoch, from a knock probe).
///
/// # Errors
///
/// [`Error::KeyPackage`] for a KeyPackage that does not validate,
/// [`Error::Group`] for a bad group id, a signature key that is not Ed25519,
/// or a signing failure.
pub fn sign_self_removal(
    old_identity: &Identity,
    group_id: &[u8],
    epoch: u64,
    replacement_key_package_wire: &[u8],
) -> Result<Vec<u8>> {
    let reference = key_package_ref(old_identity, replacement_key_package_wire)?;
    let pubkey = <[u8; KEY_LEN]>::try_from(old_identity.signature_public_key())
        .map_err(|_| Error::Group("self-removal proof: the leaf key is not Ed25519".into()))?;
    let message = statement(group_id, epoch, &pubkey, &reference)?;
    let raw = old_identity
        .signature_keys
        .sign(&message)
        .map_err(|e| Error::Group(format!("self-removal proof: sign: {e:?}")))?;
    let signature = <[u8; SIG_LEN]>::try_from(raw.as_slice())
        .map_err(|_| Error::Group("self-removal proof: the signature is not 64 bytes".into()))?;
    Ok(SelfRemovalProof {
        epoch,
        removed_signature_pubkey: pubkey,
        key_package_ref: reference,
        signature,
    }
    .encode())
}

/// Would `proof_wire` cover replacing the leaf `removed_signature_pubkey` by
/// the KeyPackage `key_package_wire` in `group_id`, in a commit leaving
/// `epoch` (the admitting sibling's current epoch)? The check the admitting
/// sibling runs BEFORE it builds the replacement commit: a commit the DS
/// takes but every peer refuses would leave the sibling ahead of them. Never
/// panics; any malformed input answers `false`.
#[must_use]
pub fn covers_key_package(
    identity: &Identity,
    proof_wire: &[u8],
    group_id: &[u8],
    epoch: u64,
    removed_signature_pubkey: &[u8],
    key_package_wire: &[u8],
) -> bool {
    let Ok(proof) = SelfRemovalProof::decode(proof_wire) else {
        return false;
    };
    let Ok(reference) = key_package_ref(identity, key_package_wire) else {
        return false;
    };
    proof.covers(
        group_id,
        epoch,
        removed_signature_pubkey,
        &[reference.to_vec()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codec_round_trips_and_refuses_other_lengths_and_versions() {
        let proof = SelfRemovalProof {
            epoch: 0x0102_0304_0506_0708,
            removed_signature_pubkey: [1; 32],
            key_package_ref: [2; 32],
            signature: [3; 64],
        };
        let wire = proof.encode();
        assert_eq!(wire.len(), SELF_REMOVAL_PROOF_LEN);
        assert_eq!(SelfRemovalProof::decode(&wire).unwrap(), proof);
        assert_eq!(&wire[1..9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(SelfRemovalProof::decode(&wire[..SELF_REMOVAL_PROOF_LEN - 1]).is_err());
        let mut longer = wire.clone();
        longer.push(0);
        assert!(SelfRemovalProof::decode(&longer).is_err());
        let mut v2 = wire;
        v2[0] = 2;
        assert!(SelfRemovalProof::decode(&v2).is_err());
        assert!(SelfRemovalProof::decode(&[]).is_err());
    }

    #[test]
    fn the_wire_length_differs_from_an_action_attestation() {
        // Both travel in the envelope's removal-authorisation slot.
        assert_ne!(SELF_REMOVAL_PROOF_LEN, lakko_attest::ATTESTATION_LEN);
    }

    #[test]
    fn a_signed_proof_verifies_under_the_leaf_key_and_nothing_else() {
        let old = Identity::generate("old").unwrap();
        let new = Identity::generate("new").unwrap();
        let kp = crate::key_package::serialize(&new.create_key_package().unwrap()).unwrap();
        let gid = [7u8; 32];
        let wire = sign_self_removal(&old, &gid, 5, &kp).unwrap();
        let proof = SelfRemovalProof::decode(&wire).unwrap();
        assert_eq!(proof.epoch, 5);
        let reference = key_package_ref(&old, &kp).unwrap().to_vec();
        let refs = std::slice::from_ref(&reference);
        assert!(proof.covers(&gid, 5, old.signature_public_key(), refs));
        // Another group, another epoch, another leaf, no matching add: refused.
        assert!(!proof.covers(&[8u8; 32], 5, old.signature_public_key(), refs));
        assert!(!proof.covers(&gid, 6, old.signature_public_key(), refs));
        assert!(!proof.covers(&gid, 4, old.signature_public_key(), refs));
        assert!(!proof.covers(&gid, 5, new.signature_public_key(), refs));
        assert!(!proof.covers(&gid, 5, old.signature_public_key(), &[vec![0u8; 32]]));
        assert!(!proof.covers(&gid, 5, old.signature_public_key(), &[]));
        // A proof whose clear epoch was rewritten no longer verifies.
        let mut moved = proof.clone();
        moved.epoch = 6;
        assert!(!moved.covers(&gid, 6, old.signature_public_key(), refs));
        // The sibling's pre-check agrees.
        assert!(covers_key_package(
            &old,
            &wire,
            &gid,
            5,
            old.signature_public_key(),
            &kp
        ));
        assert!(!covers_key_package(
            &old,
            &wire,
            &gid,
            6,
            old.signature_public_key(),
            &kp
        ));
        assert!(!covers_key_package(
            &old,
            &wire,
            &gid,
            5,
            new.signature_public_key(),
            &kp
        ));
        let other_kp = crate::key_package::serialize(&new.create_key_package().unwrap()).unwrap();
        assert!(!covers_key_package(
            &old,
            &wire,
            &gid,
            5,
            old.signature_public_key(),
            &other_kp
        ));
        assert!(!covers_key_package(
            &old,
            &wire[..10],
            &gid,
            5,
            old.signature_public_key(),
            &kp
        ));
        // A tampered signature: refused.
        let mut bad = proof.clone();
        bad.signature[0] ^= 1;
        assert!(!bad.covers(&gid, 5, old.signature_public_key(), &[reference]));
    }
}
