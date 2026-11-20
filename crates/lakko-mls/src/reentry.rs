// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A device that fell behind a conversation comes back ([ADR-0091] phase 2,
//! M1 and M2).
//!
//! A device that failed to apply the commit moving its conversation from
//! epoch N to N+1 can never read the conversation again: every later row is
//! sealed at an epoch it does not hold. Its leaf `L` is still in everyone
//! else's roster, so neither the ADR-0050 re-admission arm (it sees no
//! missing device) nor a Welcome (C-88 refuses a Welcome over an active
//! group) brings it back. ADR-0091 brings it back by ONE commit that replaces
//! `L` with a fresh KeyPackage `K` the device minted under its own device
//! identity: same signature key, same ADR-0050 binding, so the person, their
//! role and their authorship (C-68) are unchanged.
//!
//! ## M1, the re-entry marker
//!
//! `K` carries a KeyPackage extension of private-use type
//! [`REENTRY_MARKER_EXTENSION_TYPE`] (`0xFF04`):
//!
//! ```text
//! version(1)=0x01 ‖ group_id(32) ‖ epoch_left(8, BE)
//! ```
//!
//! It names the conversation and the epoch the replacement commit must
//! leave. It is authenticated by the KeyPackage's own signature, which is
//! made with the leaf key `L` already holds, so it needs no second signature
//! and rides with the `Add` to every peer (S0, 2026-11-19: OpenMLS 0.8.1
//! carries it; `KeyPackageIn::validate` requires the leaf to advertise the
//! type, which [`reentry_leaf_capabilities`] does). `K` is a last-resort
//! package so a refused re-entry join leaves it usable, and it is never
//! published to the directory.
//!
//! ## M2, the replacement arm
//!
//! Two rules, applied by the receipt gate, the sweep filter, the peer commit
//! gate and the proposer's own courtesy check:
//!
//! 1. **The collision rule.** An `Add` whose signature key is a current
//!    leaf's is admissible only as a re-entry: it carries a marker for this
//!    group and the current epoch (the epoch the commit leaves; OpenMLS
//!    empties the proposal store on every merged commit, so a stored
//!    proposal is only ever committed from the epoch it was received at),
//!    and its account is that leaf's account. Anything else is refused with
//!    [`RefusalReason::ReentryPairInvalid`]. Without this a replayed
//!    directory KeyPackage of a member could swap that member's device out.
//! 2. **The pair rule.** A `Remove(L)` that the ordinary rules refuse only as
//!    [`RefusalReason::OwnerUnremovable`],
//!    [`RefusalReason::RemoveNotOwnerOrAdmin`] or
//!    [`RefusalReason::OneToOneMembershipFixed`] is authorised when an `Add`
//!    by the SAME sender is a valid re-entry for `L`: in the proposal store
//!    at receipt, in the selection at the sweep, in the same commit at the
//!    commit gate. Those three refusals exist to keep an account in the
//!    group, and with `K` the account keeps a leaf. The commit-wide rules
//!    [`crate::group_proposals::owner_survives`] and
//!    [`crate::group_proposals::one_to_one_holds`] are unchanged and pass,
//!    because they are keyed on accounts.
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use core::result::Result as CoreResult;

use openmls::framing::Sender;
use openmls::group::QueuedProposal;
use openmls::messages::proposals::Proposal;
use openmls::prelude::{Capabilities, Ciphersuite, Extension, ExtensionType, KeyPackage};
use thiserror::Error;

use crate::group::{MemberRecord, GROUP_ID_LEN};
use crate::refusal::RefusalReason;

/// The private-use KeyPackage extension type of the re-entry marker. Beside
/// `0xFF01` (DeviceGroup removal policy), `0xFF02` (role table) and `0xFF03`
/// (conversation kind). Frozen.
pub const REENTRY_MARKER_EXTENSION_TYPE: u16 = 0xFF04;

/// The marker's schema version.
pub const REENTRY_MARKER_VERSION_V1: u8 = 0x01;

/// The marker body's one well-formed length: version, group id, epoch.
pub const REENTRY_MARKER_LEN: usize = 1 + GROUP_ID_LEN + 8;

/// The re-entry marker a re-entry KeyPackage carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReentryMarker {
    /// The conversation the device asks to come back to.
    pub group_id: [u8; GROUP_ID_LEN],
    /// The epoch the replacement commit must leave: the `latest_epoch` the
    /// device read from the journal.
    pub epoch_left: u64,
}

/// Why a marker body was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReentryMarkerError {
    /// The body is not exactly [`REENTRY_MARKER_LEN`] bytes.
    #[error("re-entry marker has the wrong length")]
    BadLength,
    /// The version byte is not [`REENTRY_MARKER_VERSION_V1`].
    #[error("re-entry marker carries unknown version {0:#04x}")]
    UnknownVersion(u8),
}

impl ReentryMarkerError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
        }
    }
}

impl ReentryMarker {
    /// Encode the marker body.
    #[must_use]
    pub fn encode(&self) -> [u8; REENTRY_MARKER_LEN] {
        let mut out = [0u8; REENTRY_MARKER_LEN];
        out[0] = REENTRY_MARKER_VERSION_V1;
        out[1..=GROUP_ID_LEN].copy_from_slice(&self.group_id);
        out[1 + GROUP_ID_LEN..].copy_from_slice(&self.epoch_left.to_be_bytes());
        out
    }

    /// Decode a marker body, bounded and fail-closed. The version is checked
    /// before the length, so an unknown later layout reports its version.
    ///
    /// # Errors
    ///
    /// [`ReentryMarkerError::UnknownVersion`] and
    /// [`ReentryMarkerError::BadLength`].
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, ReentryMarkerError> {
        let (&version, _) = bytes.split_first().ok_or(ReentryMarkerError::BadLength)?;
        if version != REENTRY_MARKER_VERSION_V1 {
            return Err(ReentryMarkerError::UnknownVersion(version));
        }
        if bytes.len() != REENTRY_MARKER_LEN {
            return Err(ReentryMarkerError::BadLength);
        }
        let mut group_id = [0u8; GROUP_ID_LEN];
        group_id.copy_from_slice(&bytes[1..=GROUP_ID_LEN]);
        let mut epoch = [0u8; 8];
        epoch.copy_from_slice(&bytes[1 + GROUP_ID_LEN..]);
        Ok(Self {
            group_id,
            epoch_left: u64::from_be_bytes(epoch),
        })
    }

    /// The marker as the KeyPackage extension that carries it.
    pub(crate) fn extension(&self) -> Extension {
        Extension::Unknown(
            REENTRY_MARKER_EXTENSION_TYPE,
            openmls::prelude::UnknownExtension(self.encode().to_vec()),
        )
    }
}

/// The marker a KeyPackage carries: `None` when it carries no `0xFF04`
/// extension, `Some(Err)` when it carries one that does not decode.
#[must_use]
pub fn marker_of_key_package(
    key_package: &KeyPackage,
) -> Option<CoreResult<ReentryMarker, ReentryMarkerError>> {
    key_package
        .extensions()
        .unknown(REENTRY_MARKER_EXTENSION_TYPE)
        .map(|body| ReentryMarker::decode(&body.0))
}

/// The leaf capabilities of a re-entry KeyPackage: the ordinary Lakko set
/// plus `0xFF04`, which `KeyPackageIn::validate` requires for a package that
/// carries the marker (S0).
pub(crate) fn reentry_leaf_capabilities(cs: Ciphersuite) -> Capabilities {
    let base = crate::device_group::lakko_leaf_capabilities(cs);
    let mut extensions: Vec<ExtensionType> = base.extensions().to_vec();
    extensions.push(ExtensionType::Unknown(REENTRY_MARKER_EXTENSION_TYPE));
    Capabilities::new(None, Some(&[cs]), Some(&extensions), None, None)
}

/// The group a re-entry is judged in: its id, its current epoch (the epoch
/// a commit built now leaves) and its roster.
#[derive(Clone, Copy)]
pub(crate) struct ReentryScope<'a> {
    /// The group id.
    pub group_id: &'a [u8],
    /// The current (pre-merge) epoch.
    pub epoch: u64,
    /// The current (pre-merge) roster.
    pub members: &'a [MemberRecord],
}

/// How an `Add`'s KeyPackage stands with the collision rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReentryAdd {
    /// Its signature key is no current leaf's: the ordinary rules apply.
    NotColliding,
    /// A valid re-entry replacing the leaf at `replaced_leaf`.
    Valid {
        /// The current leaf whose signature key the package carries.
        replaced_leaf: u32,
    },
    /// Its signature key is a current leaf's and it is not a valid re-entry
    /// for this group, this epoch and that leaf's account.
    Invalid,
}

/// Judge one KeyPackage against the collision rule.
pub(crate) fn reentry_add_of_key_package(
    scope: ReentryScope<'_>,
    key_package: &KeyPackage,
) -> ReentryAdd {
    let leaf = key_package.leaf_node();
    let signature_key = leaf.signature_key().as_slice();
    let Some(member) = scope
        .members
        .iter()
        .find(|m| m.signature_public_key == signature_key)
    else {
        return ReentryAdd::NotColliding;
    };
    let Some(Ok(marker)) = marker_of_key_package(key_package) else {
        return ReentryAdd::Invalid;
    };
    if marker.group_id.as_slice() != scope.group_id || marker.epoch_left != scope.epoch {
        return ReentryAdd::Invalid;
    }
    let account = crate::device_binding::account_key_of_credential(
        leaf.credential().serialized_content(),
        signature_key,
    );
    match account {
        Some(a) if !member.account_key.is_empty() && a.as_slice() == member.account_key => {
            ReentryAdd::Valid {
                replaced_leaf: member.leaf_index,
            }
        },
        _ => ReentryAdd::Invalid,
    }
}

/// Judge one proposal against the collision rule; anything but an `Add` is
/// [`ReentryAdd::NotColliding`].
pub(crate) fn reentry_add(scope: ReentryScope<'_>, proposal: &Proposal) -> ReentryAdd {
    match proposal {
        Proposal::Add(add) => reentry_add_of_key_package(scope, add.key_package()),
        _ => ReentryAdd::NotColliding,
    }
}

/// `true` when a refusal of a `Remove` is one the pair rule lifts.
pub(crate) const fn refusal_lifted_by_pair(reason: RefusalReason) -> bool {
    matches!(
        reason,
        RefusalReason::OwnerUnremovable
            | RefusalReason::RemoveNotOwnerOrAdmin
            | RefusalReason::OneToOneMembershipFixed
    )
}

/// `true` when one of `peers` is an `Add` by the same `sender` that is a
/// valid re-entry replacing `target_leaf`.
pub(crate) fn remove_is_paired<'a>(
    scope: ReentryScope<'_>,
    sender: &Sender,
    target_leaf: u32,
    peers: impl IntoIterator<Item = &'a QueuedProposal>,
) -> bool {
    if !matches!(sender, Sender::Member(_)) {
        return false;
    }
    peers.into_iter().any(|qp| {
        qp.sender() == sender
            && reentry_add(scope, qp.proposal())
                == ReentryAdd::Valid {
                    replaced_leaf: target_leaf,
                }
    })
}

/// How a proposal stands once the ordinary rules and the re-entry arm have
/// both been asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Authorised by the ordinary rules; not a re-entry.
    Plain,
    /// A valid re-entry `Add` replacing `replaced_leaf` (authorised by the
    /// ordinary rules too: its account is already in the group).
    ReentryAdd {
        /// The leaf it replaces.
        replaced_leaf: u32,
    },
    /// A `Remove` the ordinary rules refuse and the pair rule authorises.
    PairedRemove {
        /// The leaf it removes.
        target_leaf: u32,
    },
}

/// **The** M2 decision: the ordinary per-proposal rule
/// ([`crate::group_proposals::authorise`], and with `one_to_one_early` the
/// early one-to-one answer [`crate::group_proposals::one_to_one_allows`]),
/// the collision rule, and the pair rule, in that order. Used by the receipt
/// gate, the sweep, the peer commit gate and the proposer's own check, so the
/// four cannot drift.
///
/// `peers` are the proposals a `Remove` may be paired with: the pending
/// store at receipt, the selection at the sweep, the commit's own proposals
/// at the commit gate.
///
/// # Errors
///
/// [`RefusalReason::ReentryPairInvalid`] for a colliding `Add` that is not a
/// valid re-entry; otherwise the ordinary refusal.
pub(crate) fn judge<'a>(
    scope: ReentryScope<'_>,
    table: &crate::group_roles::RoleTable,
    kind: Option<crate::group_kind::GroupKind>,
    one_to_one_early: bool,
    sender: &Sender,
    proposal: &Proposal,
    peers: impl IntoIterator<Item = &'a QueuedProposal>,
) -> crate::Result<Standing> {
    use crate::group_proposals::{
        authorise, classify, one_to_one_allows, proposer_at, ProposalKind,
    };
    let asked = classify(proposal);
    let proposer = match sender {
        Sender::Member(idx) => proposer_at(scope.members, idx.u32()),
        _ => None,
    };
    let collision = reentry_add(scope, proposal);
    if collision == ReentryAdd::Invalid {
        return Err(crate::Error::refused(
            RefusalReason::ReentryPairInvalid,
            "add refused: its signature key is a current member's and it is not a re-entry \
             for this group, this epoch and that member's account",
        ));
    }
    let ordinary = if one_to_one_early {
        one_to_one_allows(kind, scope.members, proposer, &asked)
    } else {
        Ok(())
    }
    .and_then(|()| authorise(table, scope.members, proposer, &asked));
    match ordinary {
        Ok(()) => Ok(match collision {
            ReentryAdd::Valid { replaced_leaf } => Standing::ReentryAdd { replaced_leaf },
            ReentryAdd::NotColliding | ReentryAdd::Invalid => Standing::Plain,
        }),
        Err(e) => {
            let ProposalKind::Remove { target_leaf } = asked else {
                return Err(e);
            };
            let liftable = e.refusal_reason().is_some_and(refusal_lifted_by_pair);
            if liftable && proposer.is_some() && remove_is_paired(scope, sender, target_leaf, peers)
            {
                Ok(Standing::PairedRemove { target_leaf })
            } else {
                Err(e)
            }
        },
    }
}

impl crate::identity::Identity {
    /// Mint a re-entry KeyPackage `K` for the conversation `group_id`, naming
    /// `epoch_left`, the `latest_epoch` this device read (M1).
    ///
    /// `K` is built under this device identity: the same signature key as the
    /// device's current leaf and the same ADR-0050 device binding, so a peer
    /// can pair it with that leaf. It advertises `0xFF04` beside the ordinary
    /// Lakko capabilities and is a last-resort package, so a refused
    /// [`crate::Group::join_reentry`] leaves it usable. Its private keys stay
    /// in this identity's storage until [`Self::forget_key_package`]. The
    /// caller records `K` durably BEFORE sending the request, and keeps the
    /// last [`crate::REENTRY_REQUESTS_KEPT`] per conversation.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Group`] when `group_id` is not [`GROUP_ID_LEN`] bytes;
    /// [`crate::Error::KeyPackage`] on an OpenMLS failure.
    pub fn create_reentry_key_package(
        &self,
        group_id: &[u8],
        epoch_left: u64,
    ) -> crate::Result<KeyPackage> {
        let group_id: [u8; GROUP_ID_LEN] = group_id.try_into().map_err(|_| {
            crate::Error::Group(format!(
                "re-entry key package refused: a group id is {GROUP_ID_LEN} bytes"
            ))
        })?;
        let marker = ReentryMarker {
            group_id,
            epoch_left,
        };
        let extensions = openmls::prelude::Extensions::single(marker.extension())
            .map_err(|e| crate::Error::KeyPackage(format!("re-entry marker: {e:?}")))?;
        let bundle = KeyPackage::builder()
            .leaf_node_capabilities(reentry_leaf_capabilities(self.ciphersuite()))
            .key_package_extensions(extensions)
            .mark_as_last_resort()
            .build(
                self.ciphersuite(),
                &self.provider,
                &self.signature_keys,
                self.credential.clone(),
            )
            .map_err(|e| crate::Error::KeyPackage(format!("{e:?}")))?;
        Ok(bundle.key_package().clone())
    }

    /// Delete the private keys of a KeyPackage this identity minted (the
    /// wire form), so a superseded re-entry request can no longer open a
    /// Welcome. Deleting a package that is not stored changes nothing.
    ///
    /// # Errors
    ///
    /// [`crate::Error::KeyPackage`] when the bytes are not a valid
    /// KeyPackage, [`crate::Error::Identity`] on a storage failure.
    pub fn forget_key_package(&self, key_package_bytes: &[u8]) -> crate::Result<()> {
        use openmls_traits::storage::StorageProvider as _;
        use openmls_traits::OpenMlsProvider as _;
        let kp = crate::key_package::deserialize(key_package_bytes)?
            .validate(
                self.provider.crypto(),
                openmls::prelude::ProtocolVersion::Mls10,
            )
            .map_err(|e| crate::Error::KeyPackage(format!("validate: {e:?}")))?;
        let hash_ref = kp
            .hash_ref(self.provider.crypto())
            .map_err(|e| crate::Error::KeyPackage(format!("hash_ref: {e:?}")))?;
        self.provider
            .storage()
            .delete_key_package(&hash_ref)
            .map_err(|e| crate::Error::Identity(format!("delete key package: {e:?}")))
    }
}

/// Fuzz oracle behind `fuzz_api::decode_reentry_marker`: `None` when the
/// decoder refuses; `Some(true)` when what decoded re-encodes to the input.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let marker = ReentryMarker::decode(bytes).ok()?;
    Some(marker.encode().as_slice() == bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn marker_round_trips_at_one_length() {
        let m = ReentryMarker {
            group_id: [7u8; GROUP_ID_LEN],
            epoch_left: 0x0102_0304_0506_0708,
        };
        let bytes = m.encode();
        assert_eq!(bytes.len(), 41);
        assert_eq!(bytes[0], 0x01);
        assert_eq!(&bytes[33..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(ReentryMarker::decode(&bytes), Ok(m));
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn marker_refuses_another_version_and_length() {
        let good = ReentryMarker {
            group_id: [1u8; GROUP_ID_LEN],
            epoch_left: 9,
        }
        .encode();
        assert_eq!(
            ReentryMarker::decode(&good[..40]),
            Err(ReentryMarkerError::BadLength)
        );
        let mut long = good.to_vec();
        long.push(0);
        assert_eq!(
            ReentryMarker::decode(&long),
            Err(ReentryMarkerError::BadLength)
        );
        let mut v2 = good;
        v2[0] = 2;
        assert_eq!(
            ReentryMarker::decode(&v2),
            Err(ReentryMarkerError::UnknownVersion(2))
        );
        assert_eq!(
            ReentryMarker::decode(&[]),
            Err(ReentryMarkerError::BadLength)
        );
    }

    #[test]
    fn only_the_three_account_keeping_refusals_are_lifted() {
        for reason in RefusalReason::ALL {
            let lifted = refusal_lifted_by_pair(reason);
            let expected = matches!(
                reason,
                RefusalReason::OwnerUnremovable
                    | RefusalReason::RemoveNotOwnerOrAdmin
                    | RefusalReason::OneToOneMembershipFixed
            );
            assert_eq!(lifted, expected, "{reason:?}");
        }
    }

    proptest! {
        #[test]
        fn marker_decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }
    }
}
