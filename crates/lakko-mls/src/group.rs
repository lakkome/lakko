// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS group state machine — wraps OpenMLS's [`MlsGroup`].
//!
//! Provides a Lakko-shaped API on top of OpenMLS:
//!
//! - [`Group::create`] — start a new group with the local identity
//! - [`Group::join_from_welcome`] — join from a Welcome message
//! - [`Group::add_member`] — produce a Commit + Welcome to add a key package
//! - [`Group::remove_member`] — produce a Commit removing a leaf
//! - [`Group::create_application_message`] — encrypt application data
//! - [`Group::process_incoming`] — decrypt / advance the state machine
//! - [`Group::export_secret`] — derive a key for non-MLS uses (SFrame, etc.)

use openmls::framing::{MlsMessageBodyIn, MlsMessageIn, MlsMessageOut};
use openmls::group::{MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, StagedWelcome};
use openmls::prelude::{KeyPackage, LeafNodeIndex, ProcessedMessageContent, ProtocolVersion};
use openmls_traits::OpenMlsProvider as _;
use tls_codec::{Deserialize as _, Serialize as _};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::identity::Identity;

/// Lakko MLS conversation group.
///
/// Owns the local view of an MLS group. Per-device state lives in the
/// associated [`Identity`]'s storage provider; this struct is moved /
/// borrowed alongside that identity for every operation.
pub struct Group {
    inner: MlsGroup,
}

/// Output of [`Group::add_member`].
///
/// - `commit` is broadcast to existing group members so they can advance
///   their state to the new epoch.
/// - `welcome` is delivered to the **newly added** member so they can join.
pub struct MemberAddition {
    /// Commit — distribute to all existing members.
    pub commit: MlsMessageOut,
    /// Welcome — distribute to the newly added member only.
    pub welcome: MlsMessageOut,
}

/// One member's identity snapshot at the current epoch
/// (Phase 1.18h). Returned by [`Group::members`].
///
/// `leaf_index` is the OpenMLS-side leaf position used by
/// `remove_member` to identify which leaf to delete. Stable
/// inside a single epoch; can renumber across a remove/add
/// cycle, so callers MUST NOT cache it past an
/// [`Group::process_incoming`] of a Commit.
///
/// `signature_public_key` is the raw 32-byte Ed25519
/// verifying key the member's identity layer signs with —
/// callers use it to render a fingerprint, look the member up
/// in the contacts cache, or compare against `Identity::signature_public_key`
/// to identify "is this me".
#[derive(Debug, Clone)]
pub struct MemberRecord {
    /// OpenMLS leaf position. `u32` mirrors `LeafNodeIndex`'s
    /// internal repr.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key.
    pub signature_public_key: Vec<u8>,
}

/// Result of [`Group::process_incoming`].
#[derive(Debug)]
pub enum Incoming {
    /// Decrypted application payload.
    Application(Vec<u8>),
    /// A commit was processed and merged into the group state. The local
    /// view advanced to a new epoch.
    Commit,
    /// A standalone proposal was processed (no state advance).
    Proposal,
    /// An external join proposal was processed.
    ExternalJoinProposal,
}

impl Group {
    /// Create a new MLS group with the given identity as the founding member.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    pub fn create(identity: &Identity) -> Result<Self> {
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(identity.ciphersuite())
            // Embed the ratchet tree in Welcomes so joiners can reconstruct
            // group state without an out-of-band tree fetch. We may revisit
            // this for very large groups (Phase 4 channels) where the tree
            // can dominate Welcome size.
            .use_ratchet_tree_extension(true)
            .build();

        let inner = MlsGroup::new(
            &identity.provider,
            &identity.signature_keys,
            &cfg,
            identity.credential.clone(),
        )
        .map_err(|e| Error::Group(format!("create: {e:?}")))?;

        Ok(Self { inner })
    }

    /// Join an existing group from a Welcome message.
    ///
    /// `welcome_bytes` is the TLS-encoded Welcome (extracted by the sender
    /// from the [`MemberAddition`] returned by [`Group::add_member`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] for malformed wire data, [`Error::Unsupported`]
    /// if the message is not a Welcome, or [`Error::Group`] on OpenMLS
    /// failure (e.g. signature mismatch, ciphersuite mismatch).
    pub fn join_from_welcome(welcome_bytes: &[u8], identity: &Identity) -> Result<Self> {
        let mls_in = decode_message(welcome_bytes)?;
        let MlsMessageBodyIn::Welcome(welcome) = mls_in.extract() else {
            return Err(Error::Unsupported("expected Welcome message"));
        };

        let cfg = MlsGroupJoinConfig::default();
        let staged = StagedWelcome::new_from_welcome(&identity.provider, &cfg, welcome, None)
            .map_err(|e| Error::Group(format!("staged welcome: {e:?}")))?;
        let inner = staged
            .into_group(&identity.provider)
            .map_err(|e| Error::Group(format!("welcome -> group: {e:?}")))?;

        Ok(Self { inner })
    }

    /// Add a member by their published [`KeyPackage`].
    ///
    /// Consumes one of the new member's published KeyPackages on success.
    /// The local group state is advanced to the new epoch atomically.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure (validation,
    /// signature, storage).
    pub fn add_member(
        &mut self,
        identity: &Identity,
        key_package: KeyPackage,
    ) -> Result<MemberAddition> {
        let (commit, welcome, _group_info) = self
            .inner
            .add_members(&identity.provider, &identity.signature_keys, &[key_package])
            .map_err(|e| Error::Group(format!("add_members: {e:?}")))?;

        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(MemberAddition { commit, welcome })
    }

    /// Add a member from a wire-format `KeyPackageIn`.
    ///
    /// Convenience wrapper that deserializes the bytes via
    /// [`crate::key_package::deserialize`], validates the package against
    /// the active MLS protocol version and ciphersuite, then delegates to
    /// [`Group::add_member`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyPackage`] for validation failures.
    pub fn add_member_from_wire(
        &mut self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<MemberAddition> {
        let kp_in = crate::key_package::deserialize(key_package_bytes)?;
        let validated = kp_in
            .validate(identity.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))?;
        self.add_member(identity, validated)
    }

    /// Remove a member identified by their leaf index.
    ///
    /// Indices come from the receiving member's own group view (e.g. via
    /// [`Group::own_leaf_index`]) or from `MlsGroup::members()` once we
    /// surface that iterator.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    pub fn remove_member(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<MlsMessageOut> {
        let (commit, _welcome_for_external_joiners, _group_info) = self
            .inner
            .remove_members(&identity.provider, &identity.signature_keys, &[leaf])
            .map_err(|e| Error::Group(format!("remove_members: {e:?}")))?;

        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(commit)
    }

    /// Encrypt an application payload for the current group epoch.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Message`] on encryption failure.
    pub fn create_application_message(
        &mut self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<MlsMessageOut> {
        self.inner
            .create_message(&identity.provider, &identity.signature_keys, plaintext)
            .map_err(|e| Error::Message(format!("create_message: {e:?}")))
    }

    /// Decrypt and advance the state machine for an incoming MLS message.
    ///
    /// `bytes` is the TLS-encoded [`MlsMessageOut`] from the wire. The
    /// returned [`Incoming`] tells the caller what kind of payload arrived
    /// (application, commit, proposal).
    ///
    /// Commits are auto-merged into the local group state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] for malformed wire data, [`Error::Message`]
    /// for processing failures, or [`Error::Unsupported`] for message kinds
    /// we don't yet handle.
    pub fn process_incoming(&mut self, identity: &Identity, bytes: &[u8]) -> Result<Incoming> {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;

        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                Ok(Incoming::Application(app.into_bytes()))
            },
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                self.inner
                    .merge_staged_commit(&identity.provider, *staged)
                    .map_err(|e| Error::Group(format!("merge_staged_commit: {e:?}")))?;
                Ok(Incoming::Commit)
            },
            ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => {
                Ok(Incoming::ExternalJoinProposal)
            },
        }
    }

    /// Derive a `length`-byte secret bound to this group's current epoch.
    ///
    /// Wraps `MlsGroup::export_secret` (RFC 9420 §8.5). The output is the
    /// canonical way to bridge MLS to non-MLS protocols (SFrame media keys,
    /// channel content keys, etc.). The result is wrapped in [`Zeroizing`]
    /// so it is wiped on drop.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Exporter`] on internal failure (e.g. zero length).
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: usize,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let secret = self
            .inner
            .export_secret(identity.provider.crypto(), label, context, length)
            .map_err(|e| Error::Exporter(format!("{e:?}")))?;
        Ok(Zeroizing::new(secret))
    }

    /// Local leaf index of `identity` in the group, if present.
    #[must_use]
    pub fn own_leaf_index(&self) -> LeafNodeIndex {
        self.inner.own_leaf_index()
    }

    /// Current group epoch (advances on every Commit).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch().as_u64()
    }

    /// Number of members in the group at the current epoch.
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.inner.members().count()
    }

    /// Snapshot of every member's `(leaf_index, signature_public_key)`
    /// at the current epoch (Phase 1.18h).
    ///
    /// Returns a `Vec` rather than an iterator so the wasm-bindgen
    /// surface doesn't have to thread a streaming protocol —
    /// member sets are bounded by the application (≤ 1k for
    /// Phase 1 groups per the ROADMAP) and a one-shot allocation
    /// is cheaper than the cross-FFI dance.
    ///
    /// Ordering follows OpenMLS's internal traversal (left-to-right
    /// over the leaf nodes), which is stable across calls inside a
    /// single epoch and recomputed on every `process_incoming`
    /// commit. Callers MUST NOT cache the index for use across
    /// epochs — a remove/add cycle can renumber leaves.
    #[must_use]
    pub fn members(&self) -> Vec<MemberRecord> {
        self.inner
            .members()
            .map(|m| MemberRecord {
                leaf_index: m.index.u32(),
                signature_public_key: m.signature_key,
            })
            .collect()
    }

    /// MLS `group_id` (RFC 9420 §6.2) — the opaque identifier
    /// shared by every member of the group at every epoch.
    ///
    /// Used as the routing key for the DS group journal at
    /// `POST /ds/group/{group_id_hex}/messages`. Length is
    /// implementation-defined by OpenMLS (typically 32 bytes
    /// of randomness chosen at `Group::create`); callers
    /// should treat it as opaque bytes.
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id().as_slice().to_vec()
    }

    /// Reconstruct a `Group` from a previously persisted MLS
    /// state in `identity.provider.storage()`. Pair with
    /// [`Identity::restore_storage`] to load both halves of
    /// the snapshot back into a single live `Identity` +
    /// `Group` pair (Phase 1.16e6a).
    ///
    /// Returns:
    ///
    /// * `Ok(Some(group))` — the storage contains a group
    ///   under `group_id`; the loaded ratchet state advances
    ///   from there as if the process never restarted.
    /// * `Ok(None)` — no group with that id is in storage
    ///   (typical case: the snapshot was wiped, or the caller
    ///   is asking for a different group than was persisted).
    /// * `Err(Error::Group)` — the storage row exists but
    ///   OpenMLS rejected its contents (cipher suite mismatch,
    ///   schema drift on storage upgrade, …).
    ///
    /// # Errors
    ///
    /// Any `MlsGroup::load` failure surfaces as
    /// [`Error::Group`] with the underlying OpenMLS message
    /// preserved.
    pub fn load(identity: &Identity, group_id: &[u8]) -> Result<Option<Self>> {
        use openmls::group::GroupId;
        let id = GroupId::from_slice(group_id);
        let loaded = MlsGroup::load(identity.provider.storage(), &id)
            .map_err(|e| Error::Group(format!("load: {e:?}")))?;
        Ok(loaded.map(|inner| Self { inner }))
    }
}

/// Encode any [`MlsMessageOut`] to its TLS wire form.
///
/// # Errors
///
/// Returns [`Error::Codec`] on encoder failure.
pub fn encode_message(msg: &MlsMessageOut) -> Result<Vec<u8>> {
    msg.tls_serialize_detached()
        .map_err(|e| Error::Codec(format!("{e:?}")))
}

/// Decode a wire-form MLS message into an [`MlsMessageIn`].
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure.
fn decode_message(bytes: &[u8]) -> Result<MlsMessageIn> {
    let mut slice = bytes;
    MlsMessageIn::tls_deserialize(&mut slice).map_err(|e| Error::Codec(format!("{e:?}")))
}
