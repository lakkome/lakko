// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entering a group by an external commit, held until the Delivery Service
//! takes it ([ADR-0064] section 7, amendment of 2026-11-14), and the GroupInfo
//! a device exports so another device of the same account can enter later.
//!
//! ## Why the entry is held by Lakko and not by OpenMLS
//!
//! OpenMLS 0.8.1 merges an external commit while it builds it
//! (`ExternalCommitBuilder::finalize`), and `clear_pending_commit` is inert for
//! one. Lakko's rule is that a commit merges only on the Delivery Service's
//! word (ADR-0081), so the entry is held here instead: the group is built in a
//! throwaway provider, checked, copied into the device's provider, and marked
//! with a provider row ([`ENTRY_ROW_DOMAIN`]) that rides the storage snapshot.
//! While the row is there the group reports a staged commit:
//! [`Group::has_staged_sweep`] is `true`, [`Group::epoch`] is the epoch the
//! commit leaves, [`Group::staged_export_secret`] answers for the epoch it
//! moves to, and every current-epoch read, send and receive is refused. The
//! verdict entry points serve it: [`Group::merge_staged_sweep`] removes the
//! mark, [`Group::abandon_staged_sweep`] deletes everything the entry wrote and
//! leaves the handle holding nothing ([`Group::is_active`] is then `false`).
//!
//! ## Two builds
//!
//! OpenMLS adds a `Remove` of any leaf that carries the entering device's
//! signature key (a resync) to the external commit, and no Lakko group accepts
//! an external commit carrying anything but its `ExternalInit`. The leaves of
//! the GroupInfo's tree are not readable through OpenMLS's public API before a
//! build, so the entry first builds once under a throwaway signature key that
//! is in no tree: that build reads the roster at the GroupInfo's epoch (the
//! [`ExternalEntryView`] the caller judges) and is dropped. The real build
//! follows only when the view passes.
//!
//! ## Leaf lifetimes ([ADR-0064] residual 9)
//!
//! Both builds skip OpenMLS's lifetime check of the tree's leaves. A leaf that
//! was admitted by a KeyPackage and never committed keeps that KeyPackage's
//! lifetime forever; in a PublisherGroup that is every co-publisher leaf
//! (only owner devices commit), so a check would make every entry fail once
//! such a lifetime ran out. The leaves were checked by the members when they
//! were admitted, the GroupInfo is signed by a leaf of the same tree, and the
//! members' own gate on the entry commit is unchanged (an external commit adds
//! no KeyPackage leaf; the new leaf is a commit leaf, which carries no
//! lifetime).
//!
//! [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md

use openmls::credentials::{BasicCredential, CredentialWithKey};
use openmls::extensions::Extensions;
use openmls::framing::{MlsMessageBodyIn, MlsMessageOut};
use openmls::group::{GroupContext, MlsGroup, MlsGroupJoinConfig};
use openmls::messages::group_info::{GroupInfo, VerifiableGroupInfo};
use openmls::messages::ConfirmationTag;
use openmls::treesync::LeafNodeParameters;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use tls_codec::Deserialize as _;

use super::{decode_message, delete_stored_group, encode_message, Group};
use crate::device_binding::DeviceBinding;
use crate::error::{Error, Result};
use crate::group_roles::ROLE_EXTENSION_TYPE;
use crate::identity::Identity;
use crate::refusal::RefusalReason;

/// Provider row prefix marking a group this device entered by an external
/// commit the Delivery Service has not taken yet (`domain ‖ group_id` -> one
/// version byte). Frozen; it rides the identity's storage snapshot.
pub(crate) const ENTRY_ROW_DOMAIN: &[u8] = b"lakko/external-entry/v1\0";

/// Provider row prefix holding the GroupInfo of the epoch a staged commit
/// moves to (`domain ‖ group_id` -> the wire GroupInfo). Frozen.
pub(crate) const STAGED_GROUP_INFO_ROW_DOMAIN: &[u8] = b"lakko/staged-group-info/v1\0";

const ENTRY_ROW_VERSION: u8 = 0x01;

/// Where a device stands with a group it entered by an external commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryState {
    /// Every group that was not entered, and an entry the Delivery Service
    /// took.
    Member,
    /// Entered; the commit awaits the Delivery Service's verdict.
    Unconfirmed,
    /// The entry was abandoned: its state is deleted and the handle holds
    /// nothing.
    Abandoned,
}

/// One leaf of the roster an entry would join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntryLeaf {
    /// OpenMLS leaf position.
    pub(crate) leaf_index: u32,
    /// The leaf's signature key.
    pub(crate) signature_public_key: Vec<u8>,
    /// The account the leaf's device binding names, when it carries one that
    /// verifies for its key; `None` otherwise (an empty credential included).
    pub(crate) bound_account: Option<[u8; 32]>,
}

/// What a GroupInfo would let a device enter, read before anything is written
/// to the device's provider.
#[derive(Clone, Debug)]
pub(crate) struct ExternalEntryView {
    /// The roster at the GroupInfo's epoch.
    pub(crate) members: Vec<EntryLeaf>,
    /// The leaf that signed the GroupInfo (OpenMLS verified the signature
    /// against it).
    pub(crate) signer_leaf: u32,
    /// `true` when the group carries the ADR-0022 role table.
    pub(crate) has_role_table: bool,
}

fn row_key(domain: &[u8], group_id: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(domain.len() + group_id.len());
    key.extend_from_slice(domain);
    key.extend_from_slice(group_id);
    key
}

fn poisoned() -> Error {
    Error::Identity("storage RwLock poisoned".to_string())
}

/// The entry state recorded for `group_id` in `identity`'s provider.
pub(crate) fn stored_entry_state(identity: &Identity, group_id: &[u8]) -> Result<EntryState> {
    let map = identity
        .provider
        .storage()
        .values
        .read()
        .map_err(|_| poisoned())?;
    Ok(match map.get(&row_key(ENTRY_ROW_DOMAIN, group_id)) {
        None => EntryState::Member,
        Some(row) if row.as_slice() == [ENTRY_ROW_VERSION] => EntryState::Unconfirmed,
        Some(_) => return Err(Error::Group("external entry row is malformed".to_string())),
    })
}

fn write_row(identity: &Identity, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
    identity
        .provider
        .storage()
        .values
        .write()
        .map_err(|_| poisoned())?
        .insert(key, value);
    Ok(())
}

fn drop_row(identity: &Identity, key: &[u8]) -> Result<()> {
    identity
        .provider
        .storage()
        .values
        .write()
        .map_err(|_| poisoned())?
        .remove(key);
    Ok(())
}

/// Keep `group_info` as the GroupInfo of the epoch `group_id`'s staged commit
/// moves to.
pub(crate) fn write_staged_group_info(
    identity: &Identity,
    group_id: &[u8],
    group_info: &[u8],
) -> Result<()> {
    write_row(
        identity,
        row_key(STAGED_GROUP_INFO_ROW_DOMAIN, group_id),
        group_info.to_vec(),
    )
}

/// Forget the staged GroupInfo of `group_id`.
pub(crate) fn drop_staged_group_info(identity: &Identity, group_id: &[u8]) -> Result<()> {
    drop_row(identity, &row_key(STAGED_GROUP_INFO_ROW_DOMAIN, group_id))
}

/// Drop both rows this module keeps for `group_id` (the group is being
/// deleted).
pub(crate) fn drop_rows(identity: &Identity, group_id: &[u8]) -> Result<()> {
    drop_row(identity, &row_key(ENTRY_ROW_DOMAIN, group_id))?;
    drop_staged_group_info(identity, group_id)
}

/// The leaf that signed a wire GroupInfo message. `bytes` already decoded as
/// an `MlsMessage` carrying a GroupInfo; this reads the `signer` field that
/// OpenMLS keeps crate-private: `version(2) ‖ wire_format(2) ‖ GroupContext ‖
/// extensions ‖ confirmation_tag ‖ signer(4) ‖ signature`. The same bytes go
/// to OpenMLS, which verifies the signature against exactly this leaf.
fn group_info_signer(bytes: &[u8]) -> Result<u32> {
    let malformed = |what: &str| Error::Codec(format!("group info: {what}"));
    let mut cur = bytes
        .get(4..)
        .ok_or_else(|| malformed("shorter than its header"))?;
    GroupContext::tls_deserialize(&mut cur).map_err(|_| malformed("group context"))?;
    Extensions::<GroupInfo>::tls_deserialize(&mut cur).map_err(|_| malformed("extensions"))?;
    ConfirmationTag::tls_deserialize(&mut cur).map_err(|_| malformed("confirmation tag"))?;
    u32::tls_deserialize(&mut cur).map_err(|_| malformed("signer"))
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build()
}

fn entry_leaf_parameters(cs: openmls::prelude::Ciphersuite) -> LeafNodeParameters {
    LeafNodeParameters::builder()
        .with_capabilities(crate::device_group::lakko_leaf_capabilities(cs))
        .build()
}

/// Build an external commit for `vgi` in a throwaway provider under
/// `signer` / `credential`. Returns the provider, the merged group and the
/// commit.
fn build_in_scratch(
    vgi: VerifiableGroupInfo,
    signer: &SignatureKeyPair,
    credential: CredentialWithKey,
) -> Result<(OpenMlsRustCrypto, MlsGroup, MlsMessageOut)> {
    let scratch = OpenMlsRustCrypto::default();
    let cs = vgi.ciphersuite();
    let (group, bundle) = MlsGroup::external_commit_builder()
        .with_config(join_config())
        .skip_lifetime_validation()
        .build_group(&scratch, vgi, credential)
        .map_err(|e| Error::Group(format!("external entry: {e:?}")))?
        .leaf_node_parameters(entry_leaf_parameters(cs))
        .load_psks(scratch.storage())
        .map_err(|e| Error::Group(format!("external entry load_psks: {e:?}")))?
        .build(scratch.rand(), scratch.crypto(), signer, |_| true)
        .map_err(|e| Error::Group(format!("external entry build: {e:?}")))?
        .finalize(&scratch)
        .map_err(|e| Error::Group(format!("external entry finalize: {e:?}")))?;
    let (commit, _welcome, _group_info) = bundle.into_messages();
    Ok((scratch, group, commit))
}

fn strict_bound_account(credential: &[u8], signature_key: &[u8]) -> Option<[u8; 32]> {
    if credential.is_empty() {
        return None;
    }
    DeviceBinding::verify_for_leaf(credential, signature_key)
        .ok()
        .map(|b| b.account_pubkey)
}

impl Group {
    /// Enter the group a GroupInfo describes by an external commit carrying
    /// its `ExternalInit` alone, and HOLD it ([`EntryState::Unconfirmed`]):
    /// nothing is final until [`Self::merge_staged_sweep`] (the Delivery
    /// Service took the commit) or [`Self::abandon_staged_sweep`] (it took
    /// another). Returns the group and the wire commit.
    ///
    /// Refused before anything is written to `identity`'s provider: a message
    /// that is not a GroupInfo, one for a group other than
    /// `expected_group_id`, one in another ciphersuite, a group this device
    /// already holds (active or ended; delete it first), a tree that already
    /// carries this device's signature key (OpenMLS would add a `Remove` to
    /// the commit, which no Lakko group accepts), and whatever `admit` refuses
    /// on the view of the roster.
    pub(crate) fn stage_external_entry(
        identity: &Identity,
        group_info: &[u8],
        expected_group_id: &[u8],
        admit: impl FnOnce(&ExternalEntryView) -> Result<()>,
    ) -> Result<(Self, Vec<u8>)> {
        let MlsMessageBodyIn::GroupInfo(vgi) = decode_message(group_info)?.extract() else {
            return Err(Error::Unsupported(
                "external entry: the message is not a GroupInfo",
            ));
        };
        if vgi.group_id().as_slice() != expected_group_id {
            return Err(Error::Group(
                "external entry refused: the GroupInfo is for another group".into(),
            ));
        }
        if vgi.ciphersuite() != identity.ciphersuite() {
            return Err(Error::Group(
                "external entry refused: the GroupInfo is in another ciphersuite".into(),
            ));
        }
        let signer_leaf = group_info_signer(group_info)?;
        if MlsGroup::load(identity.provider.storage(), vgi.group_id())
            .map_err(|e| Error::Group(format!("load stored group: {e:?}")))?
            .is_some()
        {
            return Err(Error::Group(
                "external entry refused: this device already holds a group under that id; \
                 delete it first"
                    .into(),
            ));
        }

        // The view: one build under a key that is in no tree.
        let throwaway = SignatureKeyPair::new(identity.ciphersuite().signature_algorithm())
            .map_err(|e| Error::Group(format!("external entry: throwaway key: {e:?}")))?;
        let throwaway_credential = CredentialWithKey {
            credential: BasicCredential::new(Vec::new()).into(),
            signature_key: throwaway.public().into(),
        };
        let (_scratch, preview, _commit) =
            build_in_scratch(vgi.clone(), &throwaway, throwaway_credential)?;
        let own_preview_leaf = preview.own_leaf_index();
        let members: Vec<EntryLeaf> = preview
            .members()
            .filter(|m| m.index != own_preview_leaf)
            .map(|m| EntryLeaf {
                leaf_index: m.index.u32(),
                bound_account: strict_bound_account(
                    m.credential.serialized_content(),
                    &m.signature_key,
                ),
                signature_public_key: m.signature_key,
            })
            .collect();
        let view = ExternalEntryView {
            has_role_table: preview.extensions().unknown(ROLE_EXTENSION_TYPE).is_some(),
            members,
            signer_leaf,
        };
        drop(preview);
        if view
            .members
            .iter()
            .any(|m| m.signature_public_key == identity.signature_public_key())
        {
            return Err(Error::Group(
                "external entry refused: this device's leaf is still in the group; another \
                 device retires it first"
                    .into(),
            ));
        }
        if !view.members.iter().any(|m| m.leaf_index == signer_leaf) {
            return Err(Error::Group(
                "external entry refused: the GroupInfo's signer is not a leaf".into(),
            ));
        }
        admit(&view)?;

        // The entry itself.
        let (scratch, built, commit) =
            build_in_scratch(vgi, &identity.signature_keys, identity.credential.clone())?;
        let group_id = built.group_id().clone();
        drop(built);
        {
            let rows = scratch.storage().values.read().map_err(|_| poisoned())?;
            let mut values = identity
                .provider
                .storage()
                .values
                .write()
                .map_err(|_| poisoned())?;
            for (k, v) in rows.iter() {
                values.insert(k.clone(), v.clone());
            }
        }
        write_row(
            identity,
            row_key(ENTRY_ROW_DOMAIN, group_id.as_slice()),
            vec![ENTRY_ROW_VERSION],
        )?;
        let inner = MlsGroup::load(identity.provider.storage(), &group_id)
            .map_err(|e| Error::Group(format!("load entered group: {e:?}")))?
            .ok_or_else(|| {
                Error::Group("external entry: the entered group is not stored".into())
            })?;
        let mut group = Self::from_mls_group(inner);
        group.entry = EntryState::Unconfirmed;
        Ok((group, encode_message(&commit)?))
    }

    /// `true` while this group holds an entry awaiting its verdict.
    pub(crate) fn holds_unconfirmed_entry(&self) -> bool {
        self.entry == EntryState::Unconfirmed
    }

    /// The Delivery Service took the entry: keep it.
    pub(super) fn confirm_external_entry(&mut self, identity: &Identity) -> Result<bool> {
        drop_row(identity, &row_key(ENTRY_ROW_DOMAIN, &self.group_id()))?;
        self.entry = EntryState::Member;
        Ok(true)
    }

    /// The Delivery Service took another commit: delete everything the entry
    /// wrote. The handle then holds nothing.
    pub(super) fn discard_external_entry(&mut self, identity: &Identity) -> Result<bool> {
        delete_stored_group(identity, &mut self.inner)?;
        self.pending_external_joins.clear();
        self.entry = EntryState::Abandoned;
        Ok(true)
    }

    /// Refuse `op` while an entry awaits its verdict, or after it was
    /// abandoned.
    pub(super) fn refuse_unless_entry_settled(&self, op: &str) -> Result<()> {
        match self.entry {
            EntryState::Member => Ok(()),
            EntryState::Unconfirmed => Err(Error::refused(
                RefusalReason::SweepAlreadyStaged,
                format!("{op} refused: the external entry awaits the Delivery Service's verdict"),
            )),
            EntryState::Abandoned => Err(Error::Group(format!(
                "{op} refused: the external entry was abandoned and this handle holds no group"
            ))),
        }
    }

    /// The wire GroupInfo of the current epoch, with the ratchet tree and the
    /// external public key, signed by this device's leaf.
    pub(crate) fn export_group_info(&self, identity: &Identity) -> Result<Vec<u8>> {
        self.refuse_unless_entry_settled("group_info")?;
        let info = self
            .inner
            .export_group_info(identity.provider.crypto(), &identity.signature_keys, true)
            .map_err(|e| Error::Group(format!("export group info: {e:?}")))?;
        encode_message(&info)
    }

    /// The wire GroupInfo of the epoch the staged commit moves to; `None`
    /// when nothing is staged (or the staged commit was built by a path that
    /// keeps none).
    pub(crate) fn staged_group_info(&self, identity: &Identity) -> Result<Option<Vec<u8>>> {
        match self.entry {
            EntryState::Abandoned => return Ok(None),
            EntryState::Unconfirmed => {
                let info = self
                    .inner
                    .export_group_info(identity.provider.crypto(), &identity.signature_keys, true)
                    .map_err(|e| Error::Group(format!("export group info: {e:?}")))?;
                return encode_message(&info).map(Some);
            },
            EntryState::Member => {},
        }
        if self.inner.pending_commit().is_none() {
            return Ok(None);
        }
        let map = identity
            .provider
            .storage()
            .values
            .read()
            .map_err(|_| poisoned())?;
        Ok(map
            .get(&row_key(STAGED_GROUP_INFO_ROW_DOMAIN, &self.group_id()))
            .cloned())
    }

    /// The GroupContext extensions of the current epoch.
    pub(crate) fn context_extensions(&self) -> Extensions<GroupContext> {
        self.inner.extensions().clone()
    }
}

/// **Adversary.** An external commit for `group_info` under `identity`, built
/// with none of [`Group::stage_external_entry`]'s checks and nothing written
/// to `identity`'s provider: OpenMLS adds a `Remove` when `identity`'s key is
/// already a leaf. What a peer's gate must refuse.
#[cfg(test)]
pub(crate) fn forge_external_commit(identity: &Identity, group_info: &[u8]) -> Result<Vec<u8>> {
    let MlsMessageBodyIn::GroupInfo(vgi) = decode_message(group_info)?.extract() else {
        return Err(Error::Unsupported("not a GroupInfo"));
    };
    let (_scratch, _group, commit) =
        build_in_scratch(vgi, &identity.signature_keys, identity.credential.clone())?;
    encode_message(&commit)
}

/// Whether OpenMLS, checking leaf lifetimes, would build an external commit
/// from `group_info` (the check [`Group::stage_external_entry`] skips).
#[cfg(test)]
pub(crate) fn builds_with_lifetime_check(group_info: &[u8]) -> Result<bool> {
    let MlsMessageBodyIn::GroupInfo(vgi) = decode_message(group_info)?.extract() else {
        return Err(Error::Unsupported("not a GroupInfo"));
    };
    let scratch = OpenMlsRustCrypto::default();
    let key = SignatureKeyPair::new(vgi.ciphersuite().signature_algorithm())
        .map_err(|e| Error::Group(format!("{e:?}")))?;
    let credential = CredentialWithKey {
        credential: BasicCredential::new(Vec::new()).into(),
        signature_key: key.public().into(),
    };
    Ok(MlsGroup::external_commit_builder()
        .with_config(join_config())
        .build_group(&scratch, vgi, credential)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_domains_are_pinned() {
        assert_eq!(ENTRY_ROW_DOMAIN, b"lakko/external-entry/v1\0");
        assert_eq!(
            STAGED_GROUP_INFO_ROW_DOMAIN,
            b"lakko/staged-group-info/v1\0"
        );
    }

    #[test]
    fn a_short_or_truncated_group_info_is_a_codec_error() {
        assert!(group_info_signer(&[]).is_err());
        assert!(group_info_signer(&[0, 1, 0, 4]).is_err());
        assert!(group_info_signer(&[0, 1, 0, 4, 0xff, 0xff]).is_err());
    }
}
