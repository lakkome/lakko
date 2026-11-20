// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! PublisherGroup — the persistent MLS group of a channel's publishers
//! ([ADR-0010](../../../docs/adr/0010-channels-two-tier-broadcast.md) Tier 1),
//! a group of **devices** since
//! [ADR-0090](../../../docs/adr/0090-publisher-group-device-leaves.md).
//!
//! ## What it is
//!
//! Channels use a two-tier broadcast: the PublisherGroup is an MLS group whose
//! exporter is the authority for the channel's content seed, and subscribers
//! receive that seed through sealed envelopes (`lakko_channels::subscribe`).
//! The group's MLS state survives restarts in the device identity's provider,
//! so the exporter output survives with it (C-44).
//!
//! ## Leaves are bound devices (ADR-0090 section 1)
//!
//! Every leaf is a device identity carrying the ADR-0050 device binding as its
//! credential, so every device of the owner and of every co-publisher holds its
//! own leaf and one person may hold several (at most
//! [`MAX_LEAVES_PER_ACCOUNT`]). A device identity without a binding is refused
//! at every entry point, and so is a leaf whose binding does not verify; the
//! ADR-0050 resolver's legacy arm (an empty credential is an account-key leaf)
//! is not relied on here.
//!
//! The group id is still the owner's derivation ([`publisher_group_id_for`],
//! ADR-0078 section 6), computed from the owner's ACCOUNT seed, which every
//! owner device holds. A co-publisher cannot compute it; it learns the id from
//! its `/g/` capability and keeps it.
//!
//! ## The owner's account key and the commit gate (ADR-0090 section 1)
//!
//! Every holder of the group records the owner's account key next to the
//! group (a provider row, [`PUBLISHER_GROUP_OWNER_ROW_DOMAIN`]): an owner
//! device derives it from the owner account seed, a co-publisher device takes
//! it at [`PublisherGroup::join_from_welcome`] and checks it against the
//! device that sent the Welcome. Every commit this group merges passes the
//! one rule [`check_publisher_commit`], run by the committer before it stages
//! and by every peer before it merges ([`PublisherGroup::process_incoming`]):
//!
//! 1. the committer's leaf resolves to the owner's account;
//! 2. the commit carries only `Add` and `Remove` proposals (and an update
//!    path);
//! 3. every added leaf carries a device binding that verifies for it;
//! 4. after the commit no account holds more than [`MAX_LEAVES_PER_ACCOUNT`]
//!    leaves;
//! 5. the commit changes no GroupContext extension.
//!
//! ## An owner device enters from a saved GroupInfo (ADR-0064 section 7)
//!
//! One arm of the rule admits a committer that is not a member: an **external
//! commit** carrying exactly its `ExternalInit`, whose new leaf carries a
//! device binding to the owner's account, while that account holds fewer than
//! [`MAX_LEAVES_PER_ACCOUNT`] leaves. Every other external commit is refused.
//! It is how an owner device that holds no group enters, from the GroupInfo
//! another owner device saved ([`PublisherGroup::group_info`],
//! [`PublisherGroup::staged_group_info`]): [`PublisherGroup::enter`] builds
//! the commit and holds it like a staged one, so it merges only on the
//! Delivery Service's word. A co-publisher never enters this way; an owner
//! device admits its devices.
//!
//! ## Every commit is staged (ADR-0090 section 3)
//!
//! There is no eager commit: [`PublisherGroup::stage_rotate`],
//! [`PublisherGroup::stage_remove_publisher`],
//! [`PublisherGroup::stage_approve_external_join`],
//! [`PublisherGroup::stage_admit_devices`],
//! [`PublisherGroup::stage_retire_devices`] and their combination
//! [`PublisherGroup::stage_commit`] leave the group at epoch N holding the
//! commit; [`PublisherGroup::merge_staged_commit`] on the Delivery Service's
//! `201` (or the identical re-send's `200`), [`PublisherGroup::abandon_staged_commit`]
//! on its `409`. Any owner device may commit; the journal takes one commit per
//! epoch, and the loser abandons and applies the winner. A channel seed reaches
//! subscribers only from a merged commit (C-90, proposed).
//!
//! ## Threading
//!
//! Same posture as [`Group`]: not `Sync`. The FFI wraps it in a `Mutex`.

use lakko_crypto::signing::SigningKey;
use openmls::extensions::Extensions;
use openmls::group::{GroupContext, StagedCommit};
use openmls::messages::proposals::Proposal;
use openmls::prelude::{KeyPackage, LeafNodeIndex};
use openmls_traits::OpenMlsProvider as _;

use crate::device_binding::DeviceBinding;
use crate::error::{Error, Result};
use crate::group::{
    encode_message, ExternalEntryView, Group, Incoming, JoinRequest, MemberRecord, GROUP_ID_LEN,
};
use crate::group_proposals::{classify, ProposalKind, MAX_LEAVES_PER_ACCOUNT};
use crate::identity::Identity;
use crate::refusal::RefusalReason;

/// Domain separator of the step-down plaintext a publisher sends on the
/// PublisherGroup journal ([ADR-0087] section 15, note of 2026-11-05).
/// Frozen; the trailing NUL keeps it apart from every other `lakko/` domain.
///
/// [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md
pub const PUBLISHER_STEP_DOWN_DOMAIN_V1: &[u8] = b"lakko/publisher-step-down/v1\0";

/// Version byte of the step-down plaintext.
pub const PUBLISHER_STEP_DOWN_VERSION_V1: u8 = 0x01;

/// The one length of a step-down plaintext: domain, version, channel id.
pub const PUBLISHER_STEP_DOWN_LEN: usize = PUBLISHER_STEP_DOWN_DOMAIN_V1.len() + 1 + CHANNEL_ID_LEN;

/// The step-down plaintext for `channel_id`:
/// `"lakko/publisher-step-down/v1\0" ‖ 0x01 ‖ channel_id(16)`. It carries no
/// identity: who steps down is the MLS-authenticated sender of the message
/// it rides in, resolved to an account key at receipt (C-68).
#[must_use]
pub fn compose_publisher_step_down(channel_id: &ChannelId) -> Vec<u8> {
    let mut out = Vec::with_capacity(PUBLISHER_STEP_DOWN_LEN);
    out.extend_from_slice(PUBLISHER_STEP_DOWN_DOMAIN_V1);
    out.push(PUBLISHER_STEP_DOWN_VERSION_V1);
    out.extend_from_slice(channel_id.as_bytes());
    out
}

/// `true` when `plaintext` is exactly the step-down plaintext for
/// `channel_id`. Exact length, the version and the channel are checked, so a
/// step-down carried into another channel's PublisherGroup does nothing.
#[must_use]
pub fn is_publisher_step_down(plaintext: &[u8], channel_id: &ChannelId) -> bool {
    plaintext.len() == PUBLISHER_STEP_DOWN_LEN
        && plaintext == compose_publisher_step_down(channel_id)
}

/// `true` when `plaintext` has the shape of a step-down plaintext for any
/// channel (domain, version, length): the cheap recognition a background
/// filter needs to keep it out of a notification.
#[must_use]
pub fn is_publisher_step_down_envelope(plaintext: &[u8]) -> bool {
    plaintext.len() == PUBLISHER_STEP_DOWN_LEN
        && plaintext.starts_with(PUBLISHER_STEP_DOWN_DOMAIN_V1)
        && plaintext[PUBLISHER_STEP_DOWN_DOMAIN_V1.len()] == PUBLISHER_STEP_DOWN_VERSION_V1
}

/// MLS-exporter domain-separation label for channel content
/// seeds. Mirrors the value
/// `lakko-channels::seed::CHANNEL_EXPORTER_LABEL` (the two
/// constants are intentionally identical so the iOS /
/// Android composers + web composer + this Rust wrapper all
/// agree on the exporter input).
pub const CHANNEL_EXPORTER_LABEL: &str = "lakko/channel/v1";

/// Length of a `channel_id`. ADR-0010 §"Identifiers" pins
/// channels at 16 bytes.
pub const CHANNEL_ID_LEN: usize = 16;

/// Length of an account key (Ed25519 public key).
pub const ACCOUNT_KEY_LEN: usize = 32;

/// 16-byte channel identifier. Newtype so the deterministic-
/// group-id derivation has a single source-of-truth shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChannelId([u8; CHANNEL_ID_LEN]);

impl ChannelId {
    /// Wrap raw bytes. Refuses any length other than
    /// [`CHANNEL_ID_LEN`].
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] when `bytes.len() != CHANNEL_ID_LEN`.
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; CHANNEL_ID_LEN] = bytes.try_into().map_err(|_| {
            Error::Codec(format!(
                "channel_id length {} != {CHANNEL_ID_LEN}",
                bytes.len()
            ))
        })?;
        Ok(Self(arr))
    }

    /// Raw bytes for FFI / exporter context use.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; CHANNEL_ID_LEN] {
        &self.0
    }
}

/// Domain-separation context of the PublisherGroup id derivation (ADR-0078
/// section 6). Pinned; a change re-addresses every channel's PublisherGroup.
const PUBLISHER_GROUP_ID_DOMAIN: &str = "lakko/pubgrp/id/v3";

// A PublisherGroup id is a Delivery Service journal routing key, which is
// exactly 32 bytes; the derivation's output width is that.
const _: () = assert!(GROUP_ID_LEN == 32);

/// Provider row prefix under which a device records the owner's account key
/// of a PublisherGroup it holds (`domain ‖ group_id` -> 32 bytes). Frozen; it
/// rides the identity's storage snapshot like the retained join requests.
pub const PUBLISHER_GROUP_OWNER_ROW_DOMAIN: &[u8] = b"lakko/publisher-group-owner/v1\0";

/// The PublisherGroup id of the channel `channel_id` owned by the account
/// whose Ed25519 seed is `owner_account_seed`:
/// `BLAKE3-derive-key("lakko/pubgrp/id/v3", owner_account_seed ||
/// channel_id)`, exactly [`GROUP_ID_LEN`] bytes. Not computable from the
/// channel id alone.
#[must_use]
pub fn publisher_group_id_for(
    owner_account_seed: &[u8; 32],
    channel_id: &ChannelId,
) -> [u8; GROUP_ID_LEN] {
    let mut material = [0u8; 32 + CHANNEL_ID_LEN];
    material[..32].copy_from_slice(owner_account_seed);
    material[32..].copy_from_slice(channel_id.as_bytes());
    lakko_crypto::hash::blake3_derive_key_from_material(PUBLISHER_GROUP_ID_DOMAIN, &material)
}

/// The owner's account key (the Ed25519 public key of `owner_account_seed`):
/// what every owner device's binding names and what the commit gate compares
/// a committer with.
#[must_use]
pub fn publisher_owner_account_key(owner_account_seed: &[u8; 32]) -> [u8; ACCOUNT_KEY_LEN] {
    SigningKey::from_seed(owner_account_seed)
        .verifying_key()
        .to_bytes()
}

fn owner_row_key(group_id: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(PUBLISHER_GROUP_OWNER_ROW_DOMAIN.len() + group_id.len());
    key.extend_from_slice(PUBLISHER_GROUP_OWNER_ROW_DOMAIN);
    key.extend_from_slice(group_id);
    key
}

fn write_owner_row(
    identity: &Identity,
    group_id: &[u8],
    owner: &[u8; ACCOUNT_KEY_LEN],
) -> Result<()> {
    identity
        .provider
        .storage()
        .values
        .write()
        .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?
        .insert(owner_row_key(group_id), owner.to_vec());
    Ok(())
}

fn read_owner_row(identity: &Identity, group_id: &[u8]) -> Result<Option<[u8; ACCOUNT_KEY_LEN]>> {
    let map = identity
        .provider
        .storage()
        .values
        .read()
        .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?;
    match map.get(&owner_row_key(group_id)) {
        None => Ok(None),
        Some(row) => row
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| Error::Group("publisher group owner row is malformed".into())),
    }
}

fn drop_owner_row(identity: &Identity, group_id: &[u8]) -> Result<()> {
    identity
        .provider
        .storage()
        .values
        .write()
        .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?
        .remove(&owner_row_key(group_id));
    Ok(())
}

/// The account a device identity is bound to (ADR-0050). A PublisherGroup
/// leaf is a bound device; an unbound identity is refused.
fn device_account(identity: &Identity) -> Result<[u8; ACCOUNT_KEY_LEN]> {
    let Some(binding) = identity.device_binding() else {
        return Err(Error::refused(
            RefusalReason::DeviceBindingInvalid,
            "publisher group: the device identity carries no device binding",
        ));
    };
    DeviceBinding::verify_for_leaf(binding, identity.signature_public_key())
        .map(|b| b.account_pubkey)
        .map_err(|_| {
            Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "publisher group: the device binding does not verify",
            )
        })
}

/// The account a leaf's credential binds it to, refusing the legacy arm: an
/// empty credential (an account-key leaf) resolves to nothing here.
fn bound_account(credential: &[u8], signature_key: &[u8]) -> Option<[u8; ACCOUNT_KEY_LEN]> {
    if credential.is_empty() {
        return None;
    }
    DeviceBinding::verify_for_leaf(credential, signature_key)
        .ok()
        .map(|b| b.account_pubkey)
}

/// What one PublisherGroup commit does, as [`check_publisher_commit`] judges
/// it: the same view whether the committer is planning it or a peer is about
/// to merge it.
#[derive(Clone, Debug, Default)]
pub struct PublisherCommitEffects {
    /// The account the committer's leaf resolves to; `None` for a sender that
    /// is not a member or whose leaf resolves to nothing.
    pub committer_account: Option<[u8; ACCOUNT_KEY_LEN]>,
    /// `true` when the commit carries a proposal other than `Add` / `Remove`.
    pub carries_other_proposals: bool,
    /// The leaf indices the commit removes.
    pub removed: Vec<u32>,
    /// For each added leaf, the account its device binding names, or `None`
    /// when it carries no binding that verifies for it.
    pub added: Vec<Option<[u8; ACCOUNT_KEY_LEN]>>,
    /// `true` for an external commit: a sender that is not a member,
    /// carrying an `ExternalInit`. `committer_account` is then the account
    /// the new leaf's device binding names, and `carries_other_proposals` is
    /// `true` when it carries anything besides exactly one `ExternalInit`.
    pub external: bool,
    /// `true` when the commit changes the GroupContext extensions.
    pub changes_context_extensions: bool,
}

/// The PublisherGroup commit rule ([ADR-0090] section 1), run by the committer
/// before it stages and by every peer before it merges: the committer is a
/// leaf of the owner's account, the commit adds and removes only, every added
/// leaf is a bound device, and no account ends with more than
/// [`MAX_LEAVES_PER_ACCOUNT`] leaves. `pre` is the roster before the commit.
/// A commit that changes the GroupContext extensions is refused.
///
/// The one arm for a committer that is not a member ([ADR-0064] section 7,
/// amendment of 2026-11-14): an external commit carrying exactly its
/// `ExternalInit`, whose new leaf carries a device binding to the owner's
/// account, while that account holds fewer than [`MAX_LEAVES_PER_ACCOUNT`]
/// leaves. Every other external commit is refused.
///
/// [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
///
/// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
///
/// # Errors
///
/// A typed refusal: [`RefusalReason::NotOwner`] for a committer not of the
/// owner's account, [`RefusalReason::UnsupportedProposal`] for another
/// proposal kind, [`RefusalReason::DeviceBindingInvalid`] for an added leaf
/// without a verifying binding, [`RefusalReason::DeviceLimitReached`] past the
/// cap.
pub fn check_publisher_commit(
    owner_account_key: &[u8; ACCOUNT_KEY_LEN],
    pre: &[MemberRecord],
    effects: &PublisherCommitEffects,
) -> Result<()> {
    if effects.changes_context_extensions {
        return Err(Error::refused(
            RefusalReason::UnsupportedProposal,
            "publisher group commit refused: it changes the group context extensions",
        ));
    }
    if effects.external {
        return check_external_entry(owner_account_key, pre, effects);
    }
    if effects.committer_account.as_ref() != Some(owner_account_key) {
        return Err(Error::refused(
            RefusalReason::NotOwner,
            "publisher group commit refused: the committer is not a device of the owner",
        ));
    }
    if effects.carries_other_proposals {
        return Err(Error::refused(
            RefusalReason::UnsupportedProposal,
            "publisher group commit refused: it carries a proposal other than add or remove",
        ));
    }
    let mut accounts: Vec<Vec<u8>> = pre
        .iter()
        .filter(|m| !effects.removed.contains(&m.leaf_index))
        .map(|m| m.account_key.clone())
        .collect();
    for added in &effects.added {
        let Some(account) = added else {
            return Err(Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "publisher group commit refused: an added leaf is not a bound device",
            ));
        };
        accounts.push(account.to_vec());
    }
    for account in &accounts {
        if accounts.iter().filter(|a| *a == account).count() > MAX_LEAVES_PER_ACCOUNT {
            return Err(Error::refused(
                RefusalReason::DeviceLimitReached,
                format!(
                    "publisher group commit refused: an account would hold more than \
                     {MAX_LEAVES_PER_ACCOUNT} devices"
                ),
            ));
        }
    }
    Ok(())
}

/// The external-commit arm of [`check_publisher_commit`].
fn check_external_entry(
    owner_account_key: &[u8; ACCOUNT_KEY_LEN],
    pre: &[MemberRecord],
    effects: &PublisherCommitEffects,
) -> Result<()> {
    if effects.carries_other_proposals || !effects.removed.is_empty() || !effects.added.is_empty() {
        return Err(Error::refused(
            RefusalReason::UnsupportedProposal,
            "publisher group entry refused: an external commit carries its ExternalInit alone",
        ));
    }
    match effects.committer_account {
        None => Err(Error::refused(
            RefusalReason::DeviceBindingInvalid,
            "publisher group entry refused: the entering leaf is not a bound device",
        )),
        Some(account) if &account != owner_account_key => Err(Error::refused(
            RefusalReason::NotOwner,
            "publisher group entry refused: only a device of the owner enters",
        )),
        Some(_) => {
            let held = pre
                .iter()
                .filter(|m| m.account_key.as_slice() == owner_account_key.as_slice())
                .count();
            if held >= MAX_LEAVES_PER_ACCOUNT {
                return Err(Error::refused(
                    RefusalReason::DeviceLimitReached,
                    format!(
                        "publisher group entry refused: the owner already holds \
                         {MAX_LEAVES_PER_ACCOUNT} devices"
                    ),
                ));
            }
            Ok(())
        },
    }
}

/// The view [`check_publisher_commit`] takes of a received, staged commit.
/// `pre_extensions` are the GroupContext extensions before it.
fn effects_of_staged(
    pre: &[MemberRecord],
    committer_leaf: Option<u32>,
    staged: &StagedCommit,
    pre_extensions: &Extensions<GroupContext>,
) -> PublisherCommitEffects {
    let changes_context_extensions = staged.group_context().extensions() != pre_extensions;
    let external_inits = staged
        .queued_proposals()
        .filter(|qp| matches!(qp.proposal(), Proposal::ExternalInit(_)))
        .count();
    if committer_leaf.is_none() && external_inits > 0 {
        let committer_account = staged.update_path_leaf_node().and_then(|leaf| {
            bound_account(
                leaf.credential().serialized_content(),
                leaf.signature_key().as_slice(),
            )
        });
        let carries_other_proposals = external_inits != 1
            || staged
                .queued_proposals()
                .any(|qp| !matches!(qp.proposal(), Proposal::ExternalInit(_)));
        return PublisherCommitEffects {
            committer_account,
            carries_other_proposals,
            removed: Vec::new(),
            added: Vec::new(),
            external: true,
            changes_context_extensions,
        };
    }
    let committer_account = committer_leaf.and_then(|leaf| {
        pre.iter()
            .find(|m| m.leaf_index == leaf)
            .and_then(|m| <[u8; ACCOUNT_KEY_LEN]>::try_from(m.account_key.as_slice()).ok())
    });
    let carries_other_proposals = staged.queued_proposals().any(|qp| {
        !matches!(
            classify(qp.proposal()),
            ProposalKind::Add { .. } | ProposalKind::Remove { .. }
        )
    });
    let removed = staged
        .remove_proposals()
        .map(|qrp| qrp.remove_proposal().removed().u32())
        .collect();
    let added = staged
        .add_proposals()
        .map(|qap| {
            let leaf = qap.add_proposal().key_package().leaf_node();
            bound_account(
                leaf.credential().serialized_content(),
                leaf.signature_key().as_slice(),
            )
        })
        .collect();
    PublisherCommitEffects {
        committer_account,
        carries_other_proposals,
        removed,
        added,
        external: false,
        changes_context_extensions,
    }
}

/// A staged PublisherGroup commit: the commit to send through the Delivery
/// Service commit route and, when it adds leaves, the Welcome to deliver once
/// it merged.
pub struct PublisherCommit {
    /// The commit, wire-encoded.
    pub commit: Vec<u8>,
    /// The Welcome for the added leaves, wire-encoded; `None` when the commit
    /// adds nobody.
    pub welcome: Option<Vec<u8>>,
}

/// What [`PublisherGroup::enter`] returns: the group, holding its entry until
/// the Delivery Service answers, and the external commit to send.
pub struct PublisherEntry {
    /// The entered group. It reports a staged commit
    /// ([`PublisherGroup::has_staged_commit`]) until
    /// [`PublisherGroup::merge_staged_commit`] or
    /// [`PublisherGroup::abandon_staged_commit`].
    pub group: PublisherGroup,
    /// The external commit, wire-encoded.
    pub commit: Vec<u8>,
}

/// The owner's devices' rule on what they enter ([ADR-0064] section 7): a
/// group with no role table, every leaf a bound device, the GroupInfo signed
/// by a device of the owner (only owner devices commit, so only they save a
/// GroupInfo), and room for one more owner device.
///
/// [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
fn admit_entry(owner: &[u8; ACCOUNT_KEY_LEN], view: &ExternalEntryView) -> Result<()> {
    if view.has_role_table {
        return Err(Error::Group(
            "publisher group entry refused: the GroupInfo is a conversation's".into(),
        ));
    }
    if view.members.iter().any(|m| m.bound_account.is_none()) {
        return Err(Error::refused(
            RefusalReason::DeviceBindingInvalid,
            "publisher group entry refused: a leaf of the group is not a bound device",
        ));
    }
    let signer_is_owner = view
        .members
        .iter()
        .any(|m| m.leaf_index == view.signer_leaf && m.bound_account.as_ref() == Some(owner));
    if !signer_is_owner {
        return Err(Error::refused(
            RefusalReason::NotOwner,
            "publisher group entry refused: the GroupInfo was not signed by a device of the owner",
        ));
    }
    let held = view
        .members
        .iter()
        .filter(|m| m.bound_account.as_ref() == Some(owner))
        .count();
    if held >= MAX_LEAVES_PER_ACCOUNT {
        return Err(Error::refused(
            RefusalReason::DeviceLimitReached,
            format!(
                "publisher group entry refused: the owner already holds \
                 {MAX_LEAVES_PER_ACCOUNT} devices"
            ),
        ));
    }
    Ok(())
}

/// Persistent MLS-group wrapper for one Lakko channel's publisher tier.
pub struct PublisherGroup {
    inner: Group,
    channel_id: ChannelId,
    owner_account_key: [u8; ACCOUNT_KEY_LEN],
}

impl PublisherGroup {
    /// Found the PublisherGroup of `channel_id` with `device` as its only
    /// leaf ([ADR-0090] section 1). Called only by the create-channel flow and
    /// by the re-founding of section 2.5; every other open is
    /// [`Self::load`], so a device without state never founds by accident.
    ///
    /// `owner_account_seed` is the owner's account seed: it derives the group
    /// id and the owner's account key, and `device` must be bound to that
    /// account.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// [`RefusalReason::DeviceBindingInvalid`] for an unbound device or one
    /// bound to another account; [`Error::Group`] when this device already
    /// holds a group under that id, or on an OpenMLS failure.
    pub fn create(
        device: &Identity,
        owner_account_seed: &[u8; 32],
        channel_id: &ChannelId,
    ) -> Result<Self> {
        let owner = publisher_owner_account_key(owner_account_seed);
        if device_account(device)? != owner {
            return Err(Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "publisher group: the device is not bound to the owner's account",
            ));
        }
        let id = publisher_group_id_for(owner_account_seed, channel_id);
        if Group::load(device, &id)?.is_some() {
            return Err(Error::Group(
                "publisher group: this device already holds a group under that id; delete it \
                 before founding again"
                    .into(),
            ));
        }
        let inner = Group::create_with_id(device, &id)?;
        write_owner_row(device, &id, &owner)?;
        Ok(Self {
            inner,
            channel_id: *channel_id,
            owner_account_key: owner,
        })
    }

    /// Open the PublisherGroup this OWNER device holds for `channel_id`, or
    /// `Ok(None)` when it holds none (it then waits to be admitted, ADR-0090
    /// section 2.1, and never founds).
    ///
    /// # Errors
    ///
    /// [`RefusalReason::DeviceBindingInvalid`] for an unbound device or one
    /// bound to another account; propagates [`Group::load`] failures.
    pub fn load(
        device: &Identity,
        owner_account_seed: &[u8; 32],
        channel_id: &ChannelId,
    ) -> Result<Option<Self>> {
        let owner = publisher_owner_account_key(owner_account_seed);
        if device_account(device)? != owner {
            return Err(Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "publisher group: the device is not bound to the owner's account",
            ));
        }
        let id = publisher_group_id_for(owner_account_seed, channel_id);
        let Some(inner) = Group::load(device, &id)? else {
            return Ok(None);
        };
        write_owner_row(device, &id, &owner)?;
        Ok(Some(Self {
            inner,
            channel_id: *channel_id,
            owner_account_key: owner,
        }))
    }

    /// Open a PublisherGroup this device JOINED, by the id it kept: a
    /// co-publisher's device (the id came from its `/g/` link) or an owner
    /// device that knows the id. Returns `Ok(None)` when nothing is stored
    /// under `group_id`. Never creates.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] when `group_id` is not [`GROUP_ID_LEN`] bytes;
    /// [`Error::Group`] when a group is stored but the owner's account key was
    /// never recorded beside it (it cannot judge a commit, so it is not
    /// opened); propagates [`Group::load`] errors.
    pub fn load_joined(
        device: &Identity,
        group_id: &[u8],
        channel_id: &ChannelId,
    ) -> Result<Option<Self>> {
        if group_id.len() != GROUP_ID_LEN {
            return Err(Error::Codec(format!(
                "publisher group id length {} != {GROUP_ID_LEN}",
                group_id.len()
            )));
        }
        let Some(inner) = Group::load(device, group_id)? else {
            return Ok(None);
        };
        let owner = read_owner_row(device, group_id)?.ok_or_else(|| {
            Error::Group("publisher group: no owner account key is recorded for it".into())
        })?;
        Ok(Some(Self {
            inner,
            channel_id: *channel_id,
            owner_account_key: owner,
        }))
    }

    /// Join a PublisherGroup from a Welcome ([ADR-0090] section 2.3): an owner
    /// device or a co-publisher device admitted by an owner device. `group_id`
    /// is the id the device computes (owner) or kept from its `/g/` link
    /// (co-publisher), `owner_account_key` the owner's account key (the
    /// issuer of the `/g/` capability, for a co-publisher). The Welcome must
    /// come from a device of that account; a Welcome for another group, a
    /// conversation's Welcome, or one from anyone else is refused before
    /// anything is written.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// [`RefusalReason::DeviceBindingInvalid`] for an unbound device;
    /// [`RefusalReason::NotOwner`] when the Welcome's sender is not a device of
    /// `owner_account_key`; [`Error::Codec`] for a malformed id or Welcome;
    /// [`Error::Group`] for the refusals of the table-less join.
    pub fn join_from_welcome(
        device: &Identity,
        welcome_bytes: &[u8],
        group_id: &[u8],
        channel_id: &ChannelId,
        owner_account_key: &[u8; ACCOUNT_KEY_LEN],
    ) -> Result<Self> {
        if group_id.len() != GROUP_ID_LEN {
            return Err(Error::Codec(format!(
                "publisher group id length {} != {GROUP_ID_LEN}",
                group_id.len()
            )));
        }
        device_account(device)?;
        let founder = Group::peek_welcome_founder(welcome_bytes, device)?;
        if &founder.founder_account_key != owner_account_key {
            return Err(Error::refused(
                RefusalReason::NotOwner,
                "publisher group welcome refused: it was not sent by a device of the owner",
            ));
        }
        let inner = Group::join_table_less_from_welcome(welcome_bytes, device, Some(group_id))?;
        write_owner_row(device, group_id, owner_account_key)?;
        Ok(Self {
            inner,
            channel_id: *channel_id,
            owner_account_key: *owner_account_key,
        })
    }

    /// Enter the PublisherGroup of `channel_id` from a GroupInfo another owner
    /// device saved ([ADR-0064] section 7, amendment of 2026-11-14), by an
    /// external commit carrying its `ExternalInit` alone and this device's
    /// bound leaf. Nothing is final: the returned group HOLDS the entry
    /// ([`Self::has_staged_commit`] is `true`, [`Self::staged_channel_seed`]
    /// is the seed the entry moves to, [`Self::epoch`] the epoch it leaves);
    /// send the commit through the Delivery Service commit route, then
    /// [`Self::merge_staged_commit`] on its `201` / `200` or
    /// [`Self::abandon_staged_commit`] on its `409`. An abandoned entry
    /// deletes everything it wrote and the handle holds nothing; enter again
    /// from an older GroupInfo, or wait. The entry is durable: it rides the
    /// identity's storage snapshot and [`Self::load`] opens it still held.
    ///
    /// Owner devices only. Refused before anything is written: a device not
    /// bound to the owner's account, a GroupInfo that is not one, is for
    /// another group (the id `owner_account_seed` and `channel_id` derive),
    /// is a conversation's, carries a leaf that is not a bound device, was not
    /// signed by a device of the owner, or names an owner already at
    /// [`MAX_LEAVES_PER_ACCOUNT`] devices; a device that already holds a group
    /// under that id (delete it first) or whose leaf is still in the tree. A
    /// GroupInfo of an epoch the group has left builds a commit every member
    /// refuses; the Delivery Service answers it `409`.
    ///
    /// Leaf lifetimes in the GroupInfo's tree are not checked (ADR-0064
    /// residual 9): a co-publisher leaf never commits, so it keeps its
    /// KeyPackage's lifetime forever.
    ///
    /// **The returned commit is wrapped (G-55, [ADR-0078] section 4).** An
    /// external commit is a `PublicMessage`, so its new leaf's device binding
    /// would show the owner's account key to the Delivery Service next to a
    /// journal id derived to hide it. `ds_access_seed` is the Delivery
    /// Service access seed of the epoch the GroupInfo describes (the one
    /// saved beside it in the vault head); the commit leaves that epoch and
    /// is sealed under [`crate::entry_wrap`]'s key derived from it. Every
    /// member derives the same seed from its exporter and opens it in
    /// [`Self::process_incoming`]. A wrong seed is not detected here: the
    /// Delivery Service refuses the proof signed with it, and no member could
    /// open the row.
    ///
    /// [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
    /// [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md
    ///
    /// # Errors
    ///
    /// [`RefusalReason::DeviceBindingInvalid`], [`RefusalReason::NotOwner`],
    /// [`RefusalReason::DeviceLimitReached`] as above; [`Error::Codec`] /
    /// [`Error::Unsupported`] for bytes that are not a GroupInfo;
    /// [`Error::Group`] for the other refusals and OpenMLS failures.
    pub fn enter(
        device: &Identity,
        group_info: &[u8],
        owner_account_seed: &[u8; 32],
        channel_id: &ChannelId,
        ds_access_seed: &[u8; 32],
    ) -> Result<PublisherEntry> {
        let owner = publisher_owner_account_key(owner_account_seed);
        if device_account(device)? != owner {
            return Err(Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "publisher group entry refused: the device is not bound to the owner's account",
            ));
        }
        let id = publisher_group_id_for(owner_account_seed, channel_id);
        let (inner, raw_commit) =
            Group::stage_external_entry(device, group_info, &id, |view| admit_entry(&owner, view))?;
        // The held entry reports the epoch its commit leaves.
        let commit =
            crate::entry_wrap::wrap_entry_commit(ds_access_seed, &id, inner.epoch(), &raw_commit)?;
        write_owner_row(device, &id, &owner)?;
        Ok(PublisherEntry {
            group: Self {
                inner,
                channel_id: *channel_id,
                owner_account_key: owner,
            },
            commit,
        })
    }

    /// The GroupInfo of the current epoch, wire-encoded, with the ratchet tree
    /// and the external public key, signed by this device's leaf: what an
    /// owner device saves so another owner device can [`Self::enter`]
    /// ([ADR-0064] section 7).
    ///
    /// [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
    ///
    /// # Errors
    ///
    /// [`RefusalReason::SweepAlreadyStaged`] while an entry awaits its
    /// verdict; [`Error::Group`] on an abandoned entry or an OpenMLS failure.
    pub fn group_info(&self, device: &Identity) -> Result<Vec<u8>> {
        self.inner.export_group_info(device)
    }

    /// The GroupInfo of the epoch the staged commit moves to, wire-encoded,
    /// `None` when nothing is staged: what an owner device saves BEFORE it
    /// sends the commit ([ADR-0064] section 7). For an entry, the GroupInfo of
    /// the epoch the entry moves to.
    ///
    /// [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on an OpenMLS failure.
    pub fn staged_group_info(&self, device: &Identity) -> Result<Option<Vec<u8>>> {
        self.inner.staged_group_info(device)
    }

    /// Delete this device's state for the group, active or not ([ADR-0090]
    /// section 2.5: the journal was reset under it, or the account is
    /// founding the group again). The owner row and the retained join
    /// requests go with it. The device is then a reader until admitted again.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on a storage failure.
    pub fn delete(&mut self, device: &Identity) -> Result<()> {
        let group_id = self.group_id();
        self.inner.delete_regardless(device)?;
        drop_owner_row(device, &group_id)
    }

    /// 32-byte channel content-key seed derived from the MLS exporter at the
    /// current epoch: `(label, context, length)` = ([`CHANNEL_EXPORTER_LABEL`],
    /// the channel id, 32).
    ///
    /// # Errors
    ///
    /// Propagates [`Group::export_secret`] failures.
    pub fn channel_seed(&self, identity: &Identity) -> Result<[u8; 32]> {
        let bytes = self.inner.export_secret(
            identity,
            CHANNEL_EXPORTER_LABEL,
            self.channel_id.as_bytes(),
            32,
        )?;
        let mut out = [0u8; 32];
        if bytes.len() != 32 {
            return Err(Error::Group(format!(
                "exporter output length {} != 32",
                bytes.len()
            )));
        }
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// The channel id this group was opened for.
    #[must_use]
    pub fn channel_id(&self) -> &ChannelId {
        &self.channel_id
    }

    /// The underlying MLS `group_id` ([`publisher_group_id_for`]).
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id()
    }

    /// The current MLS epoch.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The owner's account key this device recorded for the group.
    #[must_use]
    pub fn owner_account_key(&self) -> &[u8; ACCOUNT_KEY_LEN] {
        &self.owner_account_key
    }

    /// `true` when `owner_account_seed` derives this group's id: the caller's
    /// account owns the channel. True on every owner device, false on a
    /// co-publisher's (it passes its own account seed).
    #[must_use]
    pub fn is_owned_by(&self, owner_account_seed: &[u8; 32]) -> bool {
        publisher_group_id_for(owner_account_seed, &self.channel_id).as_slice()
            == self.group_id().as_slice()
    }

    /// `true` when `device` is bound to the owner's account: a device that may
    /// commit in this group.
    #[must_use]
    pub fn is_owner_device(&self, device: &Identity) -> bool {
        device_account(device).is_ok_and(|a| a == self.owner_account_key)
    }

    /// Every leaf at the current epoch with its account key (ADR-0050). One
    /// person may hold several leaves; the caller groups by `account_key`.
    #[must_use]
    pub fn members(&self) -> Vec<MemberRecord> {
        self.inner.members()
    }

    /// `false` once this device applied a commit that removed its own leaf.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    /// Process one message read from the PublisherGroup journal, every commit
    /// through [`check_publisher_commit`] before it merges ([ADR-0090]
    /// section 1). A refused commit is not merged and the error says why.
    ///
    /// An external entry reaches the journal wrapped (G-55,
    /// [`crate::entry_wrap`]): a row starting with
    /// [`crate::WRAPPED_ENTRY_MAGIC`] is opened under the key derived from
    /// this epoch's Delivery Service access seed, and its content must be a
    /// commit framed for this group and epoch whose committer is not a
    /// member; it then goes through the same gate as any commit. A wrapped row
    /// that does not open is an error and is never processed raw; an external
    /// commit that arrives unwrapped is refused.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// What [`Group::process_incoming`] returns, and the typed refusals of
    /// [`check_publisher_commit`]; [`Error::Message`] for a wrapped row that
    /// does not open or does not hold an external commit, and
    /// [`RefusalReason::UnsupportedProposal`] for an unwrapped external
    /// commit.
    pub fn process_incoming(&mut self, identity: &Identity, bytes: &[u8]) -> Result<Incoming> {
        let owner = self.owner_account_key;
        let pre_extensions = self.inner.context_extensions();
        let unwrapped;
        let (row, wrapped): (&[u8], bool) = if crate::entry_wrap::is_wrapped_entry(bytes) {
            let seed = self.inner.ds_access_seed(identity)?;
            let group_id = self.group_id();
            unwrapped = crate::entry_wrap::unwrap_entry_commit(
                &seed,
                &group_id,
                self.inner.epoch(),
                bytes,
            )?;
            if !crate::group::is_external_commit_framed_for(
                &unwrapped,
                &group_id,
                self.inner.epoch(),
            ) {
                return Err(Error::Message(
                    "wrapped entry: the row does not hold an external commit for this group \
                     and epoch"
                        .into(),
                ));
            }
            (unwrapped.as_slice(), true)
        } else {
            (bytes, false)
        };
        self.inner
            .process_incoming_checked(identity, row, |pre, committer, staged| {
                let effects = effects_of_staged(pre, committer, staged, &pre_extensions);
                if effects.external != wrapped {
                    return Err(if wrapped {
                        Error::Message("wrapped entry: the row holds a commit from a member".into())
                    } else {
                        Error::refused(
                            RefusalReason::UnsupportedProposal,
                            "publisher group entry refused: an external commit reaches the \
                             journal wrapped",
                        )
                    });
                }
                check_publisher_commit(&owner, pre, &effects)
            })
    }

    /// Process one row read from this journal's **knock** queue (ADR-0053):
    /// anything that is not a stranger's external-join proposal for this group
    /// is refused before it is processed.
    ///
    /// # Errors
    ///
    /// [`Error::Message`] for any other row, plus what
    /// [`Self::process_incoming`] returns.
    pub fn process_knock(&mut self, identity: &Identity, bytes: &[u8]) -> Result<Incoming> {
        if !crate::group::is_external_join_proposal_for(bytes, &self.group_id()) {
            return Err(Error::Message(
                "knock row is not an external-join proposal for this group".to_string(),
            ));
        }
        self.process_incoming(identity, bytes)
    }

    /// Retain a co-publisher's request to join carried as a bare KeyPackage
    /// (ADR-0087 section 5); approve it with
    /// [`Self::stage_approve_external_join`].
    ///
    /// # Errors
    ///
    /// What [`Group::retain_join_request`] returns.
    pub fn retain_join_request(
        &mut self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<JoinRequest> {
        self.inner.retain_join_request(identity, key_package)
    }

    /// The signature key of every retained request to join.
    #[must_use]
    pub fn pending_external_joins(&self) -> Vec<Vec<u8>> {
        self.inner.pending_external_joins()
    }

    /// Drop a retained request to join (local, no commit).
    ///
    /// # Errors
    ///
    /// What [`Group::reject_external_join`] returns.
    pub fn reject_external_join(&mut self, identity: &Identity, proposal_ref: &[u8]) -> Result<()> {
        self.inner.reject_external_join(identity, proposal_ref)
    }

    /// Encrypt an application message for the publishers at the current
    /// epoch (the hand-over envelopes of ADR-0087 section 7, the requests of
    /// ADR-0090 section 2.2). Allowed while a commit is staged.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when MLS refuses to encrypt.
    pub fn create_application_message(
        &mut self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let msg = self.inner.create_application_message(identity, plaintext)?;
        encode_message(&msg)
    }

    /// Step down as a publisher (ADR-0087 section 15): the application message
    /// to publish, which an owner device applies as
    /// [`Self::stage_remove_publisher`] of the sender's account.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when `identity` is a device of the owner (an owner
    /// deletes the channel instead), or when MLS refuses to encrypt.
    pub fn step_down(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        if self.is_owner_device(identity) {
            return Err(Error::Group(
                "step_down: the owner does not step down".into(),
            ));
        }
        let plaintext = compose_publisher_step_down(&self.channel_id);
        self.create_application_message(identity, &plaintext)
    }

    /// Stage ONE commit that admits `admit`, removes the leaves whose
    /// signature keys are `retire`, and, when `rotate`, carries an update path
    /// ([ADR-0090] section 3: one admission set, one retirement set and the
    /// rotation in one commit). Nothing merges: see
    /// [`Self::merge_staged_commit`] and [`Self::abandon_staged_commit`].
    ///
    /// Local rules, before anything is built: this device is an owner device;
    /// every admitted leaf is a bound device of an account that already holds
    /// a leaf here (the owner's or a current publisher's); every retired key is
    /// a leaf of this group other than this device's own; and the result
    /// passes [`check_publisher_commit`], as every peer will check it.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// [`RefusalReason::NotOwner`] on a device not of the owner's account;
    /// [`RefusalReason::DeviceBindingInvalid`] for an unbound admitted leaf;
    /// [`RefusalReason::DeviceLimitReached`] past the cap; [`Error::KeyPackage`]
    /// for a KeyPackage that does not validate; [`Error::Group`] for an
    /// admitted account that does not publish here, a retired key that is not
    /// a leaf or is this device's own, an empty commit, or an OpenMLS failure;
    /// [`RefusalReason::SweepAlreadyStaged`] while a commit is staged.
    pub fn stage_commit(
        &mut self,
        identity: &Identity,
        admit: &[&[u8]],
        retire: &[&[u8]],
        rotate: bool,
    ) -> Result<PublisherCommit> {
        let key_packages = Group::validate_key_packages(identity, admit)?;
        self.stage_with(
            identity,
            key_packages,
            retire,
            rotate,
            AdmitRule::PublishingAccount,
        )
    }

    /// Stage a rotation: a commit with an update path and nothing else
    /// ([ADR-0090] section 4). Returns the commit.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// What [`Self::stage_commit`] returns.
    pub fn stage_rotate(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        Ok(self.stage_commit(identity, &[], &[], true)?.commit)
    }

    /// Stage the admission of devices of accounts that already publish here
    /// ([ADR-0090] section 2.1, 2.2). Returns the commit and the Welcome.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// What [`Self::stage_commit`] returns; [`Error::Group`] for an empty
    /// list.
    pub fn stage_admit_devices(
        &mut self,
        identity: &Identity,
        key_packages: &[&[u8]],
    ) -> Result<MemberAdditionBytes> {
        if key_packages.is_empty() {
            return Err(Error::Group(
                "stage_admit_devices: no KeyPackage given".into(),
            ));
        }
        let staged = self.stage_commit(identity, key_packages, &[], false)?;
        let welcome = staged
            .welcome
            .ok_or_else(|| Error::Group("stage_admit_devices: no Welcome built".into()))?;
        Ok(MemberAdditionBytes {
            commit: staged.commit,
            welcome,
        })
    }

    /// Stage the retirement of the leaves whose signature keys are
    /// `device_keys` ([ADR-0090] section 2.1, 2.4). Returns the commit.
    ///
    /// [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md
    ///
    /// # Errors
    ///
    /// What [`Self::stage_commit`] returns; [`Error::Group`] for an empty
    /// list.
    pub fn stage_retire_devices(
        &mut self,
        identity: &Identity,
        device_keys: &[&[u8]],
    ) -> Result<Vec<u8>> {
        if device_keys.is_empty() {
            return Err(Error::Group("stage_retire_devices: no device named".into()));
        }
        Ok(self.stage_commit(identity, &[], device_keys, false)?.commit)
    }

    /// Stage the removal of the publisher whose account key is `account_key`:
    /// every leaf of that account in one commit (ADR-0087 section 15). Never
    /// the owner's account.
    ///
    /// # Errors
    ///
    /// [`RefusalReason::OwnerUnremovable`] for the owner's account;
    /// [`Error::Group`] when no leaf belongs to it; what [`Self::stage_commit`]
    /// returns.
    pub fn stage_remove_publisher(
        &mut self,
        identity: &Identity,
        account_key: &[u8],
    ) -> Result<Vec<u8>> {
        if account_key == self.owner_account_key.as_slice() {
            return Err(Error::refused(
                RefusalReason::OwnerUnremovable,
                "remove_publisher: the owner is never removed",
            ));
        }
        let leaves: Vec<Vec<u8>> = self
            .members()
            .into_iter()
            .filter(|m| !m.account_key.is_empty() && m.account_key == account_key)
            .map(|m| m.signature_public_key)
            .collect();
        if leaves.is_empty() {
            return Err(Error::Group(
                "remove_publisher: no publisher with this account key".into(),
            ));
        }
        let refs: Vec<&[u8]> = leaves.iter().map(Vec::as_slice).collect();
        Ok(self.stage_commit(identity, &[], &refs, false)?.commit)
    }

    /// Stage the approval of a retained co-publisher request to join
    /// (ADR-0028 OQ3). The request stays retained until the commit merges.
    /// Returns the commit and the joiner's Welcome.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when no retained request matches, or the joiner is a
    /// member already; what [`Self::stage_commit`] returns.
    pub fn stage_approve_external_join(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<MemberAdditionBytes> {
        let key_package = self.inner.retained_join_for_direct_commit(proposal_ref)?;
        let staged = self.stage_with(
            identity,
            vec![key_package],
            &[],
            false,
            AdmitRule::AnyAccount,
        )?;
        let welcome = staged
            .welcome
            .ok_or_else(|| Error::Group("approve: no Welcome built".into()))?;
        Ok(MemberAdditionBytes {
            commit: staged.commit,
            welcome,
        })
    }

    fn stage_with(
        &mut self,
        identity: &Identity,
        key_packages: Vec<KeyPackage>,
        retire: &[&[u8]],
        rotate: bool,
        rule: AdmitRule,
    ) -> Result<PublisherCommit> {
        let own_account = device_account(identity)?;
        if own_account != self.owner_account_key {
            return Err(Error::refused(
                RefusalReason::NotOwner,
                "publisher group: only a device of the owner commits",
            ));
        }
        let pre = self.members();
        let own_key = identity.signature_public_key();
        let mut removals = Vec::with_capacity(retire.len());
        for key in retire {
            if *key == own_key {
                return Err(Error::Group(
                    "publisher group: a device cannot retire its own leaf; another owner device \
                     does"
                        .into(),
                ));
            }
            let Some(m) = pre
                .iter()
                .find(|m| m.signature_public_key.as_slice() == *key)
            else {
                return Err(Error::Group(
                    "publisher group: no leaf with that device key".into(),
                ));
            };
            let leaf = LeafNodeIndex::new(m.leaf_index);
            if !removals.contains(&leaf) {
                removals.push(leaf);
            }
        }
        let mut added = Vec::with_capacity(key_packages.len());
        for kp in &key_packages {
            let leaf = kp.leaf_node();
            let account = bound_account(
                leaf.credential().serialized_content(),
                leaf.signature_key().as_slice(),
            );
            if let (Some(account), AdmitRule::PublishingAccount) = (account, rule) {
                let publishes = pre.iter().any(|m| {
                    !removals.iter().any(|r| r.u32() == m.leaf_index)
                        && m.account_key.as_slice() == account.as_slice()
                });
                if !publishes {
                    return Err(Error::Group(
                        "publisher group: a device is admitted only for an account that already \
                         publishes here"
                            .into(),
                    ));
                }
            }
            added.push(account);
        }
        check_publisher_commit(
            &self.owner_account_key,
            &pre,
            &PublisherCommitEffects {
                committer_account: Some(own_account),
                carries_other_proposals: false,
                removed: removals.iter().map(LeafNodeIndex::u32).collect(),
                added,
                external: false,
                changes_context_extensions: false,
            },
        )?;
        let (commit, welcome) =
            self.inner
                .stage_table_less_commit(identity, key_packages, removals, rotate)?;
        Ok(PublisherCommit {
            commit: encode_message(&commit)?,
            welcome: welcome.as_ref().map(encode_message).transpose()?,
        })
    }

    /// `true` while a commit of this device is staged and awaiting the
    /// Delivery Service's answer.
    #[must_use]
    pub fn has_staged_commit(&self) -> bool {
        self.inner.has_staged_sweep()
    }

    /// The channel seed of the epoch the staged commit moves to, `None` when
    /// nothing is staged. Never sealed to a subscriber before the merge (C-90);
    /// a caller may only record it.
    ///
    /// # Errors
    ///
    /// [`Error::Exporter`] on an exporter failure.
    pub fn staged_channel_seed(&self, identity: &Identity) -> Result<Option<[u8; 32]>> {
        let Some(bytes) = self.inner.staged_export_secret(
            identity,
            CHANNEL_EXPORTER_LABEL,
            self.channel_id.as_bytes(),
            32,
        )?
        else {
            return Ok(None);
        };
        let mut out = [0u8; 32];
        if bytes.len() != 32 {
            return Err(Error::Group("staged exporter output length != 32".into()));
        }
        out.copy_from_slice(&bytes);
        Ok(Some(out))
    }

    /// **The journal took it** (`201`, or the identical re-send's `200`):
    /// merge the staged commit. `false` when nothing was staged.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on an OpenMLS merge failure.
    pub fn merge_staged_commit(&mut self, identity: &Identity) -> Result<bool> {
        self.inner.merge_staged_sweep(identity)
    }

    /// **The journal took another** (`409`): discard the staged commit; the
    /// group stays at N to apply the winner. Touches this group only. `false`
    /// when nothing was staged.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the discard did not take effect.
    pub fn abandon_staged_commit(&mut self, identity: &Identity) -> Result<bool> {
        let entry = self.inner.holds_unconfirmed_entry();
        let abandoned = self.inner.abandon_staged_sweep(identity)?;
        if entry {
            // The entry wrote the owner row too; the group it described is gone.
            drop_owner_row(identity, &self.group_id())?;
        }
        Ok(abandoned)
    }

    /// Borrow the underlying [`Group`] for read-only use (the Delivery Service
    /// access signers).
    #[must_use]
    pub fn inner(&self) -> &Group {
        &self.inner
    }
}

/// A staged commit that adds leaves: the commit and the Welcome, wire-encoded.
pub struct MemberAdditionBytes {
    /// The commit.
    pub commit: Vec<u8>,
    /// The Welcome for the added leaves.
    pub welcome: Vec<u8>,
}

impl From<MemberAdditionBytes> for PublisherCommit {
    fn from(m: MemberAdditionBytes) -> Self {
        Self {
            commit: m.commit,
            welcome: Some(m.welcome),
        }
    }
}

/// Who a staged admission may add: a device of an account that already
/// publishes (an admission), or any bound device (an approve, which adds a
/// new publisher).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmitRule {
    PublishingAccount,
    AnyAccount,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_seed(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    fn device(seed_tag: u8, account: &[u8; 32]) -> Identity {
        Identity::from_seed_bound(&[seed_tag; 64], account, 1_700_000_000).expect("device")
    }

    fn cid(tag: u8) -> ChannelId {
        ChannelId([tag; CHANNEL_ID_LEN])
    }

    #[test]
    fn channel_id_round_trip() {
        let bytes = [0x42u8; CHANNEL_ID_LEN];
        let id = ChannelId::try_from_slice(&bytes).unwrap();
        assert_eq!(id.as_bytes(), &bytes);
        assert!(ChannelId::try_from_slice(&[0u8; 8]).is_err());
        assert!(ChannelId::try_from_slice(&[0u8; 17]).is_err());
    }

    /// The retired derivation, `b"lakko/pubgrp/v2/" || channel_id`,
    /// recomputed here only to show the new id is not it.
    fn retired_v2_group_id(channel_id: &ChannelId) -> [u8; GROUP_ID_LEN] {
        let mut id = [0u8; GROUP_ID_LEN];
        id[..16].copy_from_slice(b"lakko/pubgrp/v2/");
        id[16..].copy_from_slice(channel_id.as_bytes());
        id
    }

    #[test]
    fn group_id_is_deterministic_and_golden() {
        let c = cid(0x01);
        let seed = [0x42u8; 32];
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(&seed);
        material[32..].copy_from_slice(c.as_bytes());
        assert_eq!(
            publisher_group_id_for(&seed, &c),
            lakko_crypto::hash::blake3_derive_key_from_material("lakko/pubgrp/id/v3", &material)
        );
        assert_ne!(
            publisher_group_id_for(&seed, &cid(0x01)),
            publisher_group_id_for(&seed, &cid(0x02))
        );
    }

    #[test]
    fn the_owner_row_domain_is_pinned() {
        assert_eq!(
            PUBLISHER_GROUP_OWNER_ROW_DOMAIN,
            b"lakko/publisher-group-owner/v1\0"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        /// ADR-0078 phase 6: the id is not computable from the channel id
        /// alone, never the retired id, and the channel id is not readable
        /// from it.
        #[test]
        fn group_id_is_not_computable_from_the_channel_id(
            channel in proptest::prelude::any::<[u8; CHANNEL_ID_LEN]>(),
            seed_a in proptest::prelude::any::<[u8; 32]>(),
            seed_b in proptest::prelude::any::<[u8; 32]>(),
        ) {
            let c = ChannelId(channel);
            let id_a = publisher_group_id_for(&seed_a, &c);
            let id_b = publisher_group_id_for(&seed_b, &c);
            if seed_a == seed_b {
                proptest::prop_assert_eq!(id_a, id_b);
            } else {
                proptest::prop_assert_ne!(id_a, id_b);
            }
            proptest::prop_assert_ne!(id_a, retired_v2_group_id(&c));
            proptest::prop_assert!(!id_a.windows(CHANNEL_ID_LEN).any(|w| w == channel));
        }
    }

    #[test]
    fn create_needs_a_device_bound_to_the_owner() {
        let seed = owner_seed(0x11);
        let unbound = Identity::from_seed(&[0x21; 64]).expect("unbound");
        let err = PublisherGroup::create(&unbound, &seed, &cid(1))
            .err()
            .expect("refused");
        assert_eq!(
            err.refusal_reason(),
            Some(RefusalReason::DeviceBindingInvalid)
        );
        let other = device(0x22, &owner_seed(0x12));
        let err = PublisherGroup::create(&other, &seed, &cid(1))
            .err()
            .expect("refused");
        assert_eq!(
            err.refusal_reason(),
            Some(RefusalReason::DeviceBindingInvalid)
        );
        let dev = device(0x23, &seed);
        let pg = PublisherGroup::create(&dev, &seed, &cid(1)).expect("created");
        assert_eq!(
            pg.group_id(),
            publisher_group_id_for(&seed, &cid(1)).to_vec()
        );
        assert_eq!(pg.owner_account_key(), &publisher_owner_account_key(&seed));
        // A second founding on the same device is refused: delete first.
        assert!(PublisherGroup::create(&dev, &seed, &cid(1)).is_err());
    }

    #[test]
    fn create_then_load_round_trips_and_a_rotation_persists() {
        let seed = owner_seed(0x13);
        let dev = device(0x24, &seed);
        assert!(PublisherGroup::load(&dev, &seed, &cid(2))
            .unwrap()
            .is_none());
        let mut pg = PublisherGroup::create(&dev, &seed, &cid(2)).expect("create");
        let before = pg.channel_seed(&dev).expect("seed");
        pg.stage_rotate(&dev).expect("stage");
        assert!(pg.has_staged_commit());
        assert_eq!(
            pg.channel_seed(&dev).expect("seed"),
            before,
            "a staged commit changes nothing"
        );
        let next = pg.staged_channel_seed(&dev).expect("staged").expect("some");
        assert!(pg.merge_staged_commit(&dev).expect("merge"));
        assert_eq!(pg.channel_seed(&dev).expect("seed"), next);
        let loaded = PublisherGroup::load(&dev, &seed, &cid(2))
            .unwrap()
            .expect("some");
        assert_eq!(loaded.channel_seed(&dev).expect("seed"), next);
        assert!(loaded.is_owned_by(&seed));
        assert!(!loaded.is_owned_by(&owner_seed(0x14)));
    }

    #[test]
    fn delete_removes_the_group_and_the_owner_row() {
        let seed = owner_seed(0x15);
        let dev = device(0x25, &seed);
        let mut pg = PublisherGroup::create(&dev, &seed, &cid(3)).expect("create");
        let gid = pg.group_id();
        pg.delete(&dev).expect("delete");
        assert!(PublisherGroup::load(&dev, &seed, &cid(3))
            .unwrap()
            .is_none());
        assert!(read_owner_row(&dev, &gid).unwrap().is_none());
        // Founding again is now allowed.
        assert!(PublisherGroup::create(&dev, &seed, &cid(3)).is_ok());
    }

    /// A peer holding the group (the founder) and the GroupInfo it saved.
    fn founded(tag: u8, seed: &[u8; 32]) -> (Identity, PublisherGroup, Vec<u8>) {
        let founder = device(tag, seed);
        let pg = PublisherGroup::create(&founder, seed, &cid(tag)).expect("create");
        let info = pg.group_info(&founder).expect("group info");
        (founder, pg, info)
    }

    fn refused_by_peer(
        pg: &mut PublisherGroup,
        peer: &Identity,
        commit: &[u8],
    ) -> Option<RefusalReason> {
        let epoch = pg.epoch();
        let members = pg.members().len();
        let err = pg.process_incoming(peer, commit).expect_err("refused");
        assert_eq!(pg.epoch(), epoch, "not merged");
        assert_eq!(pg.members().len(), members, "not merged");
        err.refusal_reason()
    }

    /// `commit` wrapped as an entering device holding the right epoch's access
    /// seed would send it (G-55): the key `peer` derives at its epoch.
    fn wrapped_for(pg: &PublisherGroup, peer: &Identity, commit: &[u8]) -> Vec<u8> {
        let seed = pg.inner.ds_access_seed(peer).expect("access seed");
        crate::entry_wrap::wrap_entry_commit(&seed, &pg.group_id(), pg.epoch(), commit)
            .expect("wrap")
    }

    #[derive(Clone, Copy, Debug)]
    enum Forger {
        OtherAccount,
        Unbound,
        ResyncOfAMember,
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(9))]

        /// The gate's external arm (ADR-0064 section 7): an external commit
        /// built with none of `enter`'s local checks is refused by a peer
        /// unless it is an owner device's ExternalInit-only entry; the owner
        /// entry built the same way is accepted.
        #[test]
        fn peers_refuse_every_external_commit_but_an_owner_entry(
            forger in proptest::prop_oneof![
                proptest::strategy::Just(Forger::OtherAccount),
                proptest::strategy::Just(Forger::Unbound),
                proptest::strategy::Just(Forger::ResyncOfAMember),
            ],
            tag in 0x30u8..0x60,
        ) {
            let seed = owner_seed(tag);
            let (founder, mut pg, info) = founded(tag, &seed);
            let (id, expected) = match forger {
                Forger::OtherAccount => (device(tag ^ 0x80, &owner_seed(tag ^ 0x40)), RefusalReason::NotOwner),
                Forger::Unbound => (
                    Identity::from_seed(&[tag ^ 0x81; 64]).expect("unbound"),
                    RefusalReason::DeviceBindingInvalid,
                ),
                // The founder's own key: OpenMLS adds a Remove of its leaf.
                Forger::ResyncOfAMember => (device(tag, &seed), RefusalReason::UnsupportedProposal),
            };
            let commit = crate::group::forge_external_commit(&id, &info).expect("forged");
            // Raw, it is refused for arriving unwrapped; wrapped, by the gate.
            proptest::prop_assert_eq!(
                refused_by_peer(&mut pg, &founder, &commit),
                Some(RefusalReason::UnsupportedProposal)
            );
            let wrapped = wrapped_for(&pg, &founder, &commit);
            proptest::prop_assert_eq!(refused_by_peer(&mut pg, &founder, &wrapped), Some(expected));

            let owner_device = device(tag ^ 0x82, &seed);
            let honest = crate::group::forge_external_commit(&owner_device, &info).expect("built");
            // G-55: an owner entry sent raw is refused, so the account key in
            // its leaf never has a reason to reach the journal in the clear.
            proptest::prop_assert_eq!(
                refused_by_peer(&mut pg, &founder, &honest),
                Some(RefusalReason::UnsupportedProposal)
            );
            let honest = wrapped_for(&pg, &founder, &honest);
            let out = pg.process_incoming(&founder, &honest).expect("an owner entry applies");
            proptest::prop_assert!(matches!(out, Incoming::Commit));
            proptest::prop_assert_eq!(pg.members().len(), 2);
        }
    }

    /// G-55: a wrapped row must hold an external entry. A member's own commit
    /// sealed under the right key is refused, never merged.
    #[test]
    fn a_wrapped_member_commit_is_refused() {
        let seed = owner_seed(0x66);
        let (founder, mut pg, _) = founded(0x66, &seed);
        let second = device(0x67, &seed);
        let kp =
            crate::key_package::serialize(&second.create_key_package().expect("kp")).expect("wire");
        let add = pg.stage_admit_devices(&founder, &[&kp]).expect("admit");
        assert!(pg.merge_staged_commit(&founder).expect("merge"));
        let gid = pg.group_id();
        let mut pg2 = PublisherGroup::join_from_welcome(
            &second,
            &add.welcome,
            &gid,
            &cid(0x66),
            &publisher_owner_account_key(&seed),
        )
        .expect("join");
        let rotation = pg2.stage_rotate(&second).expect("rotate");
        let wrapped = wrapped_for(&pg, &founder, &rotation);
        let epoch = pg.epoch();
        assert!(pg.process_incoming(&founder, &wrapped).is_err());
        assert_eq!(pg.epoch(), epoch, "not merged");
        // Refused before decryption: the same commit raw is still an
        // ordinary owner commit.
        assert!(matches!(
            pg.process_incoming(&founder, &rotation).expect("applies"),
            Incoming::Commit
        ));
    }

    /// A ninth owner leaf is refused by a peer even when the entering device
    /// skipped its own check, and refused locally by `enter`.
    #[test]
    fn a_ninth_owner_device_is_refused_by_the_peer_and_locally() {
        let seed = owner_seed(0x61);
        let (founder, mut pg, _) = founded(0x61, &seed);
        let kps: Vec<Vec<u8>> = (1..MAX_LEAVES_PER_ACCOUNT)
            .map(|i| {
                let d = device(0x61 + u8::try_from(i).expect("small"), &seed);
                crate::key_package::serialize(&d.create_key_package().expect("kp")).expect("wire")
            })
            .collect();
        let refs: Vec<&[u8]> = kps.iter().map(Vec::as_slice).collect();
        pg.stage_admit_devices(&founder, &refs)
            .expect("admit seven");
        assert!(pg.merge_staged_commit(&founder).expect("merge"));
        assert_eq!(pg.members().len(), MAX_LEAVES_PER_ACCOUNT);
        let info = pg.group_info(&founder).expect("info");
        let ninth = device(0x7F, &seed);
        let commit = crate::group::forge_external_commit(&ninth, &info).expect("forged");
        let commit = wrapped_for(&pg, &founder, &commit);
        assert_eq!(
            refused_by_peer(&mut pg, &founder, &commit),
            Some(RefusalReason::DeviceLimitReached)
        );
        let access = pg.inner.ds_access_seed(&founder).expect("seed");
        let err = PublisherGroup::enter(&ninth, &info, &seed, &cid(0x61), &access)
            .err()
            .expect("refused locally");
        assert_eq!(
            err.refusal_reason(),
            Some(RefusalReason::DeviceLimitReached)
        );
        assert!(Group::load(&ninth, &pg.group_id()).expect("load").is_none());
    }

    /// ADR-0064 residual 9: a co-publisher leaf keeps its KeyPackage's
    /// lifetime forever (it never commits). Once that ran out OpenMLS's own
    /// lifetime check refuses the GroupInfo; `enter` skips it, and the member
    /// still applies the entry.
    #[test]
    fn an_entry_is_not_stopped_by_an_expired_co_publisher_leaf() {
        let seed = owner_seed(0x62);
        let (founder, mut pg, _) = founded(0x62, &seed);
        let copub = device(0x63, &owner_seed(0x64));
        let kp = crate::key_package::serialize(&copub.short_lived_key_package(2).expect("kp"))
            .expect("wire");
        pg.retain_join_request(&founder, &kp).expect("retain");
        pg.stage_approve_external_join(&founder, copub.signature_public_key())
            .expect("approve");
        assert!(pg.merge_staged_commit(&founder).expect("merge"));
        std::thread::sleep(std::time::Duration::from_secs(3));
        let info = pg.group_info(&founder).expect("info");
        assert!(
            !crate::group::builds_with_lifetime_check(&info).expect("built or not"),
            "OpenMLS's lifetime check refuses the expired co-publisher leaf"
        );
        let entering = device(0x65, &seed);
        let access = pg.inner.ds_access_seed(&founder).expect("seed");
        let entry =
            PublisherGroup::enter(&entering, &info, &seed, &cid(0x62), &access).expect("entered");
        let out = pg
            .process_incoming(&founder, &entry.commit)
            .expect("the member applies it");
        assert!(matches!(out, Incoming::Commit));
        assert_eq!(
            entry.group.staged_channel_seed(&entering).expect("seed"),
            Some(pg.channel_seed(&founder).expect("seed"))
        );
    }

    #[test]
    fn the_gate_refuses_what_it_must() {
        let owner = [0xAA; 32];
        let other = [0xBB; 32];
        let pre: Vec<MemberRecord> = (0..8)
            .map(|i| MemberRecord {
                leaf_index: i,
                signature_public_key: vec![u8::try_from(i).unwrap_or(0); 32],
                account_key: owner.to_vec(),
            })
            .collect();
        let ok = PublisherCommitEffects {
            committer_account: Some(owner),
            ..Default::default()
        };
        assert!(check_publisher_commit(&owner, &pre, &ok).is_ok());
        let not_owner = PublisherCommitEffects {
            committer_account: Some(other),
            ..Default::default()
        };
        assert_eq!(
            check_publisher_commit(&owner, &pre, &not_owner)
                .unwrap_err()
                .refusal_reason(),
            Some(RefusalReason::NotOwner)
        );
        let none = PublisherCommitEffects::default();
        assert!(check_publisher_commit(&owner, &pre, &none).is_err());
        let other_kind = PublisherCommitEffects {
            committer_account: Some(owner),
            carries_other_proposals: true,
            ..Default::default()
        };
        assert_eq!(
            check_publisher_commit(&owner, &pre, &other_kind)
                .unwrap_err()
                .refusal_reason(),
            Some(RefusalReason::UnsupportedProposal)
        );
        let unbound = PublisherCommitEffects {
            committer_account: Some(owner),
            added: vec![None],
            ..Default::default()
        };
        assert_eq!(
            check_publisher_commit(&owner, &pre, &unbound)
                .unwrap_err()
                .refusal_reason(),
            Some(RefusalReason::DeviceBindingInvalid)
        );
        let ninth = PublisherCommitEffects {
            committer_account: Some(owner),
            added: vec![Some(owner)],
            ..Default::default()
        };
        assert_eq!(
            check_publisher_commit(&owner, &pre, &ninth)
                .unwrap_err()
                .refusal_reason(),
            Some(RefusalReason::DeviceLimitReached)
        );
        let swap = PublisherCommitEffects {
            committer_account: Some(owner),
            removed: vec![7],
            added: vec![Some(owner)],
            ..Default::default()
        };
        assert!(check_publisher_commit(&owner, &pre, &swap).is_ok());
    }
}
