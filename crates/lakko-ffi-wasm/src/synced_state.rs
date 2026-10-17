// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's synced state (ADR-0058) — wasm-bindgen surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/synced_state.rs` over the same
//! [`lakko_sync::SyncedState`] fold. wasm-bindgen cannot carry Rust enums
//! with data or nested records ergonomically, so the shapes differ from
//! UniFFI while the semantics do not:
//!
//! - a folder edit is built with the [`FolderChangeSet`] class (`addMeta`,
//!   `addMember`, `addPins`, `addRemoved`, `setOrder`), folded locally with
//!   [`SyncedState::apply_folder_changes`] and sent with
//!   `DeviceGroup.createDeviceGroupFolderChangesMessage`;
//! - a conversation reference kind is the wire number: `1` one-to-one,
//!   `2` group, `3` channel; a member state is `0` none, `1` included,
//!   `2` excluded;
//! - a received payload is folded with [`SyncedState::apply_payload`] on the
//!   `ParsedDeviceGroupPayload` `parseDeviceGroupPayload` returned, whatever
//!   its kind.
//!
//! ## Saved messages (ADR-0065 section 4)
//!
//! A saved message is one more register of the same state and rides in the
//! same change set: `addSaved` / `addSavedRemoved` on a [`FolderChangeSet`]
//! (at most `savedMaxPerChangeSet()` per change set), folded locally with
//! `applyFolderChanges` and sent with
//! `DeviceGroup.createDeviceGroupFolderChangesMessage`. A received one
//! arrives through `applyPayload` (kind `folderChanges`), and a late device
//! gets the list in the snapshot parts. Read with `saved()`, `isSaved` and
//! `savedBlobIds()`.
//!
//! Blob ids cross this boundary **flat-concatenated**: one `Uint8Array`
//! holding N ids of 32 bytes each, in and out. A length that is not a
//! multiple of 32 is refused.

use std::sync::Mutex;

use lakko_sync::synced::{
    self as core, ConversationRef, ConversationRefKind, FolderChanges, MemberState, Stamped,
    SyncEntry,
};
use wasm_bindgen::prelude::*;

use crate::mls::ParsedDeviceGroupPayload;

fn js(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn fid(bytes: &[u8]) -> Result<core::FolderId, JsError> {
    core::folder_id_from_slice(bytes).map_err(js)
}

fn cref(kind: u8, id: &[u8]) -> Result<ConversationRef, JsError> {
    let r = ConversationRef {
        kind: ConversationRefKind::from_wire(i64::from(kind)).map_err(js)?,
        id: id.to_vec(),
    };
    r.validate().map_err(js)?;
    Ok(r)
}

/// Split flat-concatenated blob ids into 32-byte ids.
fn split_blob_ids(flat: &[u8]) -> Result<Vec<&[u8]>, core::BoundsError> {
    if flat.len() % core::SAVED_BLOB_ID_LEN != 0 {
        return Err(core::BoundsError::BlobIdLength);
    }
    Ok(flat.chunks_exact(core::SAVED_BLOB_ID_LEN).collect())
}

/// Build the stamped register that saves a message. The whole of
/// `addSaved` minus the JS error type, so it runs in host tests.
#[allow(clippy::too_many_arguments)]
fn saved_present(
    hlc: u64,
    ref_kind: u8,
    ref_id: &[u8],
    message_id: u64,
    saved_at_ms: u64,
    author: String,
    excerpt: String,
    attachment_kind: String,
    blob_ids: &[u8],
) -> Result<Stamped<core::SavedMessage>, core::BoundsError> {
    let conversation = ConversationRef {
        kind: ConversationRefKind::from_wire(i64::from(ref_kind))?,
        id: ref_id.to_vec(),
    };
    let value = core::SavedMessage::present(
        conversation,
        message_id,
        saved_at_ms,
        &core::SavedSnapshot {
            author,
            excerpt,
            attachment_kind,
        },
        &split_blob_ids(blob_ids)?,
    )?;
    Ok(Stamped { hlc, value })
}

/// Build the stamped register that removes a message from saved.
fn saved_removed(
    hlc: u64,
    ref_kind: u8,
    ref_id: &[u8],
    message_id: u64,
) -> Result<Stamped<core::SavedMessage>, core::BoundsError> {
    let conversation = ConversationRef {
        kind: ConversationRefKind::from_wire(i64::from(ref_kind))?,
        id: ref_id.to_vec(),
    };
    Ok(Stamped {
        hlc,
        value: core::SavedMessage::removed(conversation, message_id)?,
    })
}

/// A conversation a folder names. `kind` is the wire number (`1`
/// one-to-one, `2` group, `3` channel); `id` the MLS group id, or the
/// channel id.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderConversationRef {
    kind: u8,
    id: Vec<u8>,
}

impl From<ConversationRef> for FolderConversationRef {
    fn from(r: ConversationRef) -> Self {
        Self {
            kind: r.kind.to_wire(),
            id: r.id,
        }
    }
}

#[wasm_bindgen]
impl FolderConversationRef {
    /// `1` one-to-one, `2` group, `3` channel.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> u8 {
        self.kind
    }

    /// The MLS group id, or the channel id.
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> Vec<u8> {
        self.id.clone()
    }
}

/// A live folder edit under construction: only the registers that changed,
/// each with its own stamp. At most 1024 registers.
#[wasm_bindgen]
#[derive(Clone, Debug, Default)]
pub struct FolderChangeSet {
    pub(crate) inner: FolderChanges,
}

#[wasm_bindgen]
impl FolderChangeSet {
    /// An empty change set.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a folder's name and toggles. `includeKinds`: bit0 one-to-one,
    /// bit1 groups, bit2 channels. `excludeFlags`: bit0 muted, bit1 read,
    /// bit2 archived. Throws on a bad folder id; the name and bits are
    /// checked by [`Self::validate`] and by every consumer.
    #[wasm_bindgen(js_name = addMeta)]
    pub fn add_meta(
        &mut self,
        hlc: u64,
        folder_id: &[u8],
        name: String,
        include_kinds: u32,
        exclude_flags: u32,
    ) -> Result<(), JsError> {
        self.inner.metas.push(Stamped {
            hlc,
            value: core::FolderMeta {
                folder_id: fid(folder_id)?,
                name,
                include_kinds,
                exclude_flags,
            },
        });
        Ok(())
    }

    /// Write one conversation's membership: `state` `0` none (the way to
    /// take a chat out of the hand-added or hand-excluded set), `1`
    /// included, `2` excluded.
    #[wasm_bindgen(js_name = addMember)]
    pub fn add_member(
        &mut self,
        hlc: u64,
        folder_id: &[u8],
        ref_kind: u8,
        ref_id: &[u8],
        state: u8,
    ) -> Result<(), JsError> {
        self.inner.members.push(Stamped {
            hlc,
            value: core::FolderMember {
                folder_id: fid(folder_id)?,
                conversation: cref(ref_kind, ref_id)?,
                state: MemberState::from_wire(i64::from(state)).map_err(js)?,
            },
        });
        Ok(())
    }

    /// Write a folder's whole pin list (at most 10), top first.
    /// `refKinds[i]` / `refIds[i]` are one reference; the lengths must
    /// agree.
    #[wasm_bindgen(js_name = addPins)]
    pub fn add_pins(
        &mut self,
        hlc: u64,
        folder_id: &[u8],
        ref_kinds: &[u8],
        ref_ids: Vec<js_sys::Uint8Array>,
    ) -> Result<(), JsError> {
        if ref_kinds.len() != ref_ids.len() {
            return Err(JsError::new("refKinds and refIds differ in length"));
        }
        let pinned = ref_kinds
            .iter()
            .zip(ref_ids.iter())
            .map(|(k, id)| cref(*k, &id.to_vec()))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner.pins.push(Stamped {
            hlc,
            value: core::FolderPins {
                folder_id: fid(folder_id)?,
                pinned,
            },
        });
        Ok(())
    }

    /// Delete a folder (a tombstone at `hlc`).
    #[wasm_bindgen(js_name = addRemoved)]
    pub fn add_removed(&mut self, hlc: u64, folder_id: &[u8]) -> Result<(), JsError> {
        self.inner.removed.push(Stamped {
            hlc,
            value: fid(folder_id)?,
        });
        Ok(())
    }

    /// Write the whole folder order (at most 256 ids), first tab first.
    /// Replaces an order set earlier on this change set.
    #[wasm_bindgen(js_name = setOrder)]
    pub fn set_order(
        &mut self,
        hlc: u64,
        folder_ids: Vec<js_sys::Uint8Array>,
    ) -> Result<(), JsError> {
        let folder_ids = folder_ids
            .iter()
            .map(|f| fid(&f.to_vec()))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner.order = Some(Stamped {
            hlc,
            value: core::FolderOrder { folder_ids },
        });
        Ok(())
    }

    /// Save a message (ADR-0065 section 4), stamped `hlc`.
    ///
    /// Pass what the client holds: `author` and `excerpt` are cut to their
    /// bounds (255 and 512 bytes) on a character boundary, so the whole
    /// message text may be passed as `excerpt`; a blob id named twice is
    /// kept once. `attachmentKind` is a short token, empty when the message
    /// has no attachment. `blobIds` is the 32-byte ids of the message's
    /// files and thumbnails, flat-concatenated (may be empty).
    ///
    /// Throws when `blobIds` is not a multiple of 32 bytes or names more
    /// than 128 distinct ids, `attachmentKind` is longer than 32 bytes, or
    /// the conversation reference is not well formed. Nothing is added
    /// then.
    #[wasm_bindgen(js_name = addSaved)]
    #[allow(clippy::too_many_arguments)]
    pub fn add_saved(
        &mut self,
        hlc: u64,
        ref_kind: u8,
        ref_id: &[u8],
        message_id: u64,
        saved_at_ms: u64,
        author: String,
        excerpt: String,
        attachment_kind: String,
        blob_ids: &[u8],
    ) -> Result<(), JsError> {
        self.inner.saved.push(
            saved_present(
                hlc,
                ref_kind,
                ref_id,
                message_id,
                saved_at_ms,
                author,
                excerpt,
                attachment_kind,
                blob_ids,
            )
            .map_err(js)?,
        );
        Ok(())
    }

    /// Remove a message from saved (a tombstone at `hlc`, kept on every
    /// device; saving the message again later brings it back). Throws on a
    /// conversation reference that is not well formed.
    #[wasm_bindgen(js_name = addSavedRemoved)]
    pub fn add_saved_removed(
        &mut self,
        hlc: u64,
        ref_kind: u8,
        ref_id: &[u8],
        message_id: u64,
    ) -> Result<(), JsError> {
        self.inner
            .saved
            .push(saved_removed(hlc, ref_kind, ref_id, message_id).map_err(js)?);
        Ok(())
    }

    /// Number of registers carried.
    #[wasm_bindgen(getter, js_name = registerCount)]
    pub fn register_count(&self) -> u32 {
        u32::try_from(self.inner.register_count()).unwrap_or(u32::MAX)
    }

    /// Throw when the change set breaks an ADR-0058 section 4 bound (name,
    /// bitfields, pin / order / register caps) or a saved-message bound.
    pub fn validate(&self) -> Result<(), JsError> {
        self.inner.validate().map_err(js)
    }
}

/// One part of the catch-up re-send (ADR-0058 section 5). Get parts from
/// [`SyncedState::snapshot_parts`]; send each with
/// `DeviceGroup.createDeviceGroupSnapshotMessage`.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct SyncedSnapshotPart {
    pub(crate) inner: core::StateSnapshot,
}

#[wasm_bindgen]
impl SyncedSnapshotPart {
    /// 0-based part index.
    #[wasm_bindgen(getter)]
    pub fn part(&self) -> u32 {
        self.inner.part
    }

    /// Number of parts in this re-send.
    #[wasm_bindgen(getter)]
    pub fn parts(&self) -> u32 {
        self.inner.parts
    }

    /// Number of registers this part carries.
    #[wasm_bindgen(getter, js_name = entryCount)]
    pub fn entry_count(&self) -> u32 {
        u32::try_from(self.inner.stamped_entries().len()).unwrap_or(u32::MAX)
    }
}

/// One folder as the client shows it.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct FolderView {
    inner: core::FolderView,
}

#[wasm_bindgen]
impl FolderView {
    /// 16 bytes.
    #[wasm_bindgen(getter, js_name = folderId)]
    pub fn folder_id(&self) -> Vec<u8> {
        self.inner.folder_id.to_vec()
    }

    /// Stamp of the meta register. A folder is listed only once its name
    /// has arrived, so the meta getters always read a real value.
    #[wasm_bindgen(getter, js_name = metaHlc)]
    pub fn meta_hlc(&self) -> u64 {
        self.inner.meta.hlc
    }

    /// The folder name.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        self.inner.meta.value.name.clone()
    }

    /// Kind toggles (bit0 one-to-one, bit1 groups, bit2 channels).
    #[wasm_bindgen(getter, js_name = includeKinds)]
    pub fn include_kinds(&self) -> u32 {
        self.inner.meta.value.include_kinds
    }

    /// Exclude toggles (bit0 muted, bit1 read, bit2 archived).
    #[wasm_bindgen(getter, js_name = excludeFlags)]
    pub fn exclude_flags(&self) -> u32 {
        self.inner.meta.value.exclude_flags
    }

    /// Conversations added by hand, sorted by `(kind, id)`.
    pub fn included(&self) -> Vec<FolderConversationRef> {
        self.inner
            .included
            .iter()
            .cloned()
            .map(Into::into)
            .collect()
    }

    /// Conversations excluded by hand, sorted by `(kind, id)`.
    pub fn excluded(&self) -> Vec<FolderConversationRef> {
        self.inner
            .excluded
            .iter()
            .cloned()
            .map(Into::into)
            .collect()
    }

    /// The folder's pins, in stored order.
    pub fn pinned(&self) -> Vec<FolderConversationRef> {
        self.inner.pinned.iter().cloned().map(Into::into).collect()
    }
}

/// One saved message as the Saved screen lists it (ADR-0065 section 4).
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct SavedMessageView {
    inner: core::SavedView,
}

#[wasm_bindgen]
impl SavedMessageView {
    /// HLC stamp the save was written at.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }

    /// The conversation the message is in.
    #[wasm_bindgen(getter)]
    pub fn conversation(&self) -> FolderConversationRef {
        self.inner.conversation.clone().into()
    }

    /// The conversation's kind: `1` one-to-one, `2` group, `3` channel.
    #[wasm_bindgen(getter, js_name = refKind)]
    pub fn ref_kind(&self) -> u8 {
        self.inner.conversation.kind.to_wire()
    }

    /// The conversation's MLS group id, or channel id.
    #[wasm_bindgen(getter, js_name = refId)]
    pub fn ref_id(&self) -> Vec<u8> {
        self.inner.conversation.id.clone()
    }

    /// The message's Delivery Service journal id.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.inner.message_id
    }

    /// Wall-clock millis of the save.
    #[wasm_bindgen(getter, js_name = savedAtMs)]
    pub fn saved_at_ms(&self) -> u64 {
        self.inner.saved_at_ms
    }

    /// The author's display name as shown when saved.
    #[wasm_bindgen(getter)]
    pub fn author(&self) -> String {
        self.inner.snapshot.author.clone()
    }

    /// The start of the message text.
    #[wasm_bindgen(getter)]
    pub fn excerpt(&self) -> String {
        self.inner.snapshot.excerpt.clone()
    }

    /// The attachment kind token; empty when the message has none.
    #[wasm_bindgen(getter, js_name = attachmentKind)]
    pub fn attachment_kind(&self) -> String {
        self.inner.snapshot.attachment_kind.clone()
    }

    /// The blob ids of the message's files and thumbnails,
    /// flat-concatenated (32 bytes each).
    #[wasm_bindgen(getter, js_name = blobIds)]
    pub fn blob_ids(&self) -> Vec<u8> {
        self.inner.blob_ids.concat()
    }
}

/// A conversation's flags, stamped.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct FlagsRegister {
    inner: Stamped<core::FlagsEntry>,
}

#[wasm_bindgen]
impl FlagsRegister {
    /// HLC stamp.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The conversation's MLS group id.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.value.group_id.clone()
    }
    /// Pinned.
    #[wasm_bindgen(getter)]
    pub fn pinned(&self) -> bool {
        self.inner.value.pinned
    }
    /// Muted.
    #[wasm_bindgen(getter)]
    pub fn muted(&self) -> bool {
        self.inner.value.muted
    }
    /// Archived.
    #[wasm_bindgen(getter)]
    pub fn archived(&self) -> bool {
        self.inner.value.archived
    }
    /// Marked unread by hand.
    #[wasm_bindgen(getter, js_name = unreadOverride)]
    pub fn unread_override(&self) -> bool {
        self.inner.value.unread_override
    }
}

/// A conversation's read cursor, stamped.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct ReadCursorRegister {
    inner: Stamped<core::ReadCursorEntry>,
}

#[wasm_bindgen]
impl ReadCursorRegister {
    /// HLC stamp.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The conversation's MLS group id.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.value.group_id.clone()
    }
    /// Read up to this message id.
    #[wasm_bindgen(getter, js_name = upToMessageId)]
    pub fn up_to_message_id(&self) -> Vec<u8> {
        self.inner.value.up_to_message_id.clone()
    }
    /// Wall-clock millis of the read. Display only.
    #[wasm_bindgen(getter, js_name = readAtMs)]
    pub fn read_at_ms(&self) -> u64 {
        self.inner.value.read_at_ms
    }
}

/// A conversation's draft, stamped. Empty `text` = cleared at `hlc`.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct DraftRegister {
    inner: Stamped<core::DraftEntry>,
}

#[wasm_bindgen]
impl DraftRegister {
    /// HLC stamp.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The conversation's MLS group id.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.value.group_id.clone()
    }
    /// Composer text.
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.inner.value.text.clone()
    }
    /// Replying to this message id, or empty.
    #[wasm_bindgen(getter, js_name = replyToId)]
    pub fn reply_to_id(&self) -> Vec<u8> {
        self.inner.value.reply_to_id.clone()
    }
}

/// The explicit conversation order, stamped.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct ConversationOrderRegister {
    inner: Stamped<core::ConversationOrderEntry>,
}

#[wasm_bindgen]
impl ConversationOrderRegister {
    /// HLC stamp.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// Group ids, top first.
    #[wasm_bindgen(js_name = groupIds)]
    pub fn group_ids(&self) -> Vec<js_sys::Uint8Array> {
        self.inner
            .value
            .group_ids
            .iter()
            .map(|g| js_sys::Uint8Array::from(g.as_slice()))
            .collect()
    }
}

/// One conversation-lineage register (ADR-0073 R.5): the group
/// `successorGroupId` continues the conversation `rootConversationId` (the
/// group id of the first group of the lineage).
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineageRegister {
    inner: Stamped<core::LineageEntry>,
}

#[wasm_bindgen]
impl LineageRegister {
    /// HLC stamp the register was written at.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// MLS group id of the group that continues the conversation.
    #[wasm_bindgen(getter, js_name = successorGroupId)]
    pub fn successor_group_id(&self) -> Vec<u8> {
        self.inner.value.successor_group_id.clone()
    }
    /// The conversation id every person-facing store is keyed on.
    #[wasm_bindgen(getter, js_name = rootConversationId)]
    pub fn root_conversation_id(&self) -> Vec<u8> {
        self.inner.value.root_conversation_id.clone()
    }
}

/// The account's recovery email register as held (ADR-0075, amended
/// 2026-10-13), a removal included.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEmailRegister {
    inner: Stamped<core::RecoveryEmailEntry>,
}

#[wasm_bindgen]
impl RecoveryEmailRegister {
    /// HLC stamp the register was written at.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The normalised address, or `undefined` when the register records
    /// that the account has no recovery email (a removal).
    #[wasm_bindgen(getter)]
    pub fn address(&self) -> Option<String> {
        Some(self.inner.value.address.clone()).filter(|a| !a.is_empty())
    }
}

/// The account's own-username register as held (ADR-0076 section 3), a
/// stamped "none" included.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnUsernameRegister {
    inner: Stamped<core::OwnUsernameEntry>,
}

#[wasm_bindgen]
impl OwnUsernameRegister {
    /// HLC stamp the register was written at.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The canonical username, or `undefined` when the register records
    /// that the account has no username.
    #[wasm_bindgen(getter)]
    pub fn username(&self) -> Option<String> {
        Some(self.inner.value.username.clone()).filter(|u| !u.is_empty())
    }
}

/// Which ledger an invite link lives in (ADR-0079).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRecordKind {
    /// A contact invite link (`/i/`).
    Contact = 1,
    /// A call invite link (`/c/`).
    Call = 2,
}

/// One of the account's invite links (ADR-0079 section 4). The link secret
/// is SECRET: never log it.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRecord {
    inner: Stamped<core::LinkRecordEntry>,
}

#[wasm_bindgen]
impl LinkRecord {
    /// A record for a link just minted: no use yet, not revoked. `linkId`
    /// 16 bytes, `linkSecret` 12, `ledgerId` 6 or empty (an inline link),
    /// `maxUses` as the capability carries it (0 = single use,
    /// `0xFFFFFFFF` = no limit), `expiryMs` 0 = never.
    ///
    /// # Errors
    ///
    /// Throws `"bounds: ..."` for a field out of bounds.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        link_id: &[u8],
        link_secret: &[u8],
        kind: LinkRecordKind,
        ledger_id: &[u8],
        expiry_ms: u64,
        max_uses: u32,
        minted_at_ms: u64,
        label: String,
    ) -> Result<LinkRecord, JsError> {
        let value = core::LinkRecordEntry {
            link_id: <[u8; 16]>::try_from(link_id)
                .map_err(|_| JsError::new("bounds: link id must be 16 bytes"))?,
            link_secret: <[u8; 12]>::try_from(link_secret)
                .map_err(|_| JsError::new("bounds: link secret must be 12 bytes"))?,
            kind: match kind {
                LinkRecordKind::Contact => core::LinkRecordKind::Contact,
                LinkRecordKind::Call => core::LinkRecordKind::Call,
            },
            ledger_id: ledger_id.to_vec(),
            expiry_ms,
            max_uses,
            uses: 0,
            revoked: false,
            minted_at_ms,
            label,
            answered: Vec::new(),
        };
        value
            .validate()
            .map_err(|e| JsError::new(&format!("bounds: {e}")))?;
        Ok(Self {
            inner: Stamped { hlc: 0, value },
        })
    }

    /// HLC stamp the register was written at (0 for a record not yet
    /// written).
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.inner.hlc
    }
    /// The link's 16-byte id.
    #[wasm_bindgen(getter, js_name = linkId)]
    pub fn link_id(&self) -> Vec<u8> {
        self.inner.value.link_id.to_vec()
    }
    /// The link's 12-byte secret. SECRET.
    #[wasm_bindgen(getter, js_name = linkSecret)]
    pub fn link_secret(&self) -> Vec<u8> {
        self.inner.value.link_secret.to_vec()
    }
    /// Which ledger.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> LinkRecordKind {
        match self.inner.value.kind {
            core::LinkRecordKind::Contact => LinkRecordKind::Contact,
            core::LinkRecordKind::Call => LinkRecordKind::Call,
        }
    }
    /// The 6-byte ledger id, or empty for an inline link.
    #[wasm_bindgen(getter, js_name = ledgerId)]
    pub fn ledger_id(&self) -> Vec<u8> {
        self.inner.value.ledger_id.clone()
    }
    /// UNIX millis after which the link no longer works; 0 = never.
    #[wasm_bindgen(getter, js_name = expiryMs)]
    pub fn expiry_ms(&self) -> u64 {
        self.inner.value.expiry_ms
    }
    /// The use limit as the capability carries it.
    #[wasm_bindgen(getter, js_name = maxUses)]
    pub fn max_uses(&self) -> u32 {
        self.inner.value.max_uses
    }
    /// Requests accepted through the link so far.
    #[wasm_bindgen(getter)]
    pub fn uses(&self) -> u32 {
        self.inner.value.uses
    }
    /// Revoked (final).
    #[wasm_bindgen(getter)]
    pub fn revoked(&self) -> bool {
        self.inner.value.revoked
    }
    /// UNIX millis at which the link was minted.
    #[wasm_bindgen(getter, js_name = mintedAtMs)]
    pub fn minted_at_ms(&self) -> u64 {
        self.inner.value.minted_at_ms
    }
    /// The owner's label, or empty.
    #[wasm_bindgen(getter)]
    pub fn label(&self) -> String {
        self.inner.value.label.clone()
    }
    /// `true` when the link still works at `nowMs`.
    #[wasm_bindgen(js_name = isLive)]
    pub fn is_live(&self, now_ms: u64) -> bool {
        self.inner.value.is_live(now_ms)
    }
}

/// Canonicalise what a caller passed to `setOwnUsername`: `None` for "no
/// username" (absent or blank), else the one username rule.
fn canonical_own_username(
    username: Option<&str>,
) -> Result<Option<String>, lakko_discovery_client::UsernameError> {
    match username.map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => lakko_discovery_client::validate_username(raw).map(Some),
    }
}

/// Normalise what a caller passed to `setRecoveryEmail`: `None` for a
/// removal (absent or blank), else the one address rule. The error is the
/// `"<code>: <reason>"` message `normalizeRecoveryEmail` throws.
fn normalised_recovery_email(address: Option<&str>) -> Result<Option<String>, String> {
    match address.map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => lakko_mnemonic::recovery_email::normalize_recovery_email(raw)
            .map(Some)
            .map_err(|e| format!("{}: {e}", e.code())),
    }
}

/// The folded synced state of this device (ADR-0058 section 3).
///
/// Commutative, associative and idempotent: feed it every DeviceGroup
/// payload in whatever order they arrive, and every local write (a device
/// never receives its own messages back), and two devices that have seen
/// the same writes hold the same state.
#[wasm_bindgen]
pub struct SyncedState {
    inner: Mutex<core::SyncedState>,
}

impl SyncedState {
    fn with<T>(&self, f: impl FnOnce(&mut core::SyncedState) -> T) -> Result<T, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(f(&mut g))
    }

    fn lineage_envelopes_inner(&self) -> Result<Vec<Vec<u8>>, JsError> {
        self.with(|s| s.lineage_envelopes())
    }

    fn apply_one(&self, hlc: u64, entry: SyncEntry) -> Result<bool, JsError> {
        self.with(|s| s.apply(hlc, entry))?
            .map(|o| matches!(o, core::ApplyOutcome::Applied))
            .map_err(js)
    }
}

#[wasm_bindgen]
impl SyncedState {
    /// An empty state.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(core::SyncedState::new()),
        }
    }

    /// Restore a state persisted with [`Self::to_bytes`] or carried in a
    /// backup payload. Throws on a malformed or out-of-bounds entry: the
    /// whole state is refused.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8]) -> Result<SyncedState, JsError> {
        Ok(Self {
            inner: Mutex::new(core::SyncedState::from_bytes(bytes).map_err(js)?),
        })
    }

    /// Persist the whole state (also what a backup payload carries).
    #[wasm_bindgen(js_name = toBytes)]
    pub fn to_bytes(&self) -> Result<Vec<u8>, JsError> {
        self.with(|s| s.to_bytes())
    }

    /// Fold every register `other` holds into this state, each with its
    /// original stamp (a restore merges the backup's state this way).
    /// Returns whether anything changed.
    #[wasm_bindgen(js_name = mergeFrom)]
    pub fn merge_from(&self, other: &SyncedState) -> Result<bool, JsError> {
        // The other state's lock is released before this one is taken, so
        // merging a state into itself cannot deadlock.
        let entries = other.with(|o| o.entries())?;
        self.with(|s| s.apply_all(entries))
    }

    /// Fold a parsed DeviceGroup payload in, whatever its kind. Returns
    /// whether anything changed.
    #[wasm_bindgen(js_name = applyPayload)]
    pub fn apply_payload(&self, payload: &ParsedDeviceGroupPayload) -> Result<bool, JsError> {
        let entries = payload.stamped_entries();
        self.with(|s| s.apply_all(entries))
    }

    /// Fold a local folder edit, or a local Save / Remove of a saved
    /// message, in (the change set the client then sends). Throws when it
    /// breaks a bound.
    #[wasm_bindgen(js_name = applyFolderChanges)]
    pub fn apply_folder_changes(&self, changes: &FolderChangeSet) -> Result<bool, JsError> {
        changes.inner.validate().map_err(js)?;
        let entries = changes.inner.stamped_entries();
        self.with(|s| s.apply_all(entries))
    }

    /// Fold a local flags write in (the same values sent with
    /// `createDeviceGroupFlagsMessage` at the same `hlc`).
    #[wasm_bindgen(js_name = applyFlags)]
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn apply_flags(
        &self,
        hlc: u64,
        group_id: &[u8],
        pinned: bool,
        muted: bool,
        archived: bool,
        unread_override: bool,
    ) -> Result<bool, JsError> {
        self.apply_one(
            hlc,
            SyncEntry::Flags(core::FlagsEntry {
                group_id: group_id.to_vec(),
                pinned,
                muted,
                archived,
                unread_override,
            }),
        )
    }

    /// Fold a local read-cursor write in.
    #[wasm_bindgen(js_name = applyReadCursor)]
    pub fn apply_read_cursor(
        &self,
        hlc: u64,
        group_id: &[u8],
        up_to_message_id: &[u8],
        read_at_ms: u64,
    ) -> Result<bool, JsError> {
        self.apply_one(
            hlc,
            SyncEntry::ReadCursor(core::ReadCursorEntry {
                group_id: group_id.to_vec(),
                up_to_message_id: up_to_message_id.to_vec(),
                read_at_ms,
            }),
        )
    }

    /// Fold a local draft write in (empty `text` clears).
    #[wasm_bindgen(js_name = applyDraft)]
    pub fn apply_draft(
        &self,
        hlc: u64,
        group_id: &[u8],
        text: String,
        reply_to_id: &[u8],
    ) -> Result<bool, JsError> {
        self.apply_one(
            hlc,
            SyncEntry::Draft(core::DraftEntry {
                group_id: group_id.to_vec(),
                text,
                reply_to_id: reply_to_id.to_vec(),
            }),
        )
    }

    /// Fold a local conversation-order write in.
    #[wasm_bindgen(js_name = applyConversationOrder)]
    pub fn apply_conversation_order(
        &self,
        hlc: u64,
        group_ids: Vec<js_sys::Uint8Array>,
    ) -> Result<bool, JsError> {
        self.apply_one(
            hlc,
            SyncEntry::ConversationOrder(core::ConversationOrderEntry {
                group_ids: group_ids.iter().map(js_sys::Uint8Array::to_vec).collect(),
            }),
        )
    }

    /// Every live folder, in the resolved order.
    pub fn folders(&self) -> Result<Vec<FolderView>, JsError> {
        self.with(|s| {
            s.folders()
                .into_iter()
                .map(|inner| FolderView { inner })
                .collect()
        })
    }

    /// Flags held for a conversation, or `undefined`.
    pub fn flags(&self, group_id: &[u8]) -> Result<Option<FlagsRegister>, JsError> {
        self.with(|s| s.flags(group_id).map(|inner| FlagsRegister { inner }))
    }

    /// Read cursor held for a conversation, or `undefined`.
    #[wasm_bindgen(js_name = readCursor)]
    pub fn read_cursor(&self, group_id: &[u8]) -> Result<Option<ReadCursorRegister>, JsError> {
        self.with(|s| {
            s.read_cursor(group_id)
                .map(|inner| ReadCursorRegister { inner })
        })
    }

    /// Draft held for a conversation (a cleared one included), or
    /// `undefined`.
    pub fn draft(&self, group_id: &[u8]) -> Result<Option<DraftRegister>, JsError> {
        self.with(|s| s.draft(group_id).map(|inner| DraftRegister { inner }))
    }

    /// Every flags register.
    #[wasm_bindgen(js_name = allFlags)]
    pub fn all_flags(&self) -> Result<Vec<FlagsRegister>, JsError> {
        self.with(|s| {
            s.all_flags()
                .into_iter()
                .map(|inner| FlagsRegister { inner })
                .collect()
        })
    }

    /// Every read-cursor register.
    #[wasm_bindgen(js_name = allReadCursors)]
    pub fn all_read_cursors(&self) -> Result<Vec<ReadCursorRegister>, JsError> {
        self.with(|s| {
            s.all_read_cursors()
                .into_iter()
                .map(|inner| ReadCursorRegister { inner })
                .collect()
        })
    }

    /// Every draft register, cleared ones included.
    #[wasm_bindgen(js_name = allDrafts)]
    pub fn all_drafts(&self) -> Result<Vec<DraftRegister>, JsError> {
        self.with(|s| {
            s.all_drafts()
                .into_iter()
                .map(|inner| DraftRegister { inner })
                .collect()
        })
    }

    /// The explicit conversation order, or `undefined`.
    #[wasm_bindgen(js_name = conversationOrder)]
    pub fn conversation_order(&self) -> Result<Option<ConversationOrderRegister>, JsError> {
        self.with(|s| {
            s.conversation_order()
                .map(|inner| ConversationOrderRegister { inner })
        })
    }

    /// Record that `successorGroupId` continues the conversation
    /// `rootConversationId` (ADR-0073 R.5), stamped `hlc`. **Write-once**:
    /// when a register is already held for this successor nothing is written
    /// and `false` is returned, whatever root it names (read it with
    /// `lineageRoot`). On `true`, send `lineageEnvelopeFor(successorGroupId)`
    /// to the account's other devices with
    /// `DeviceGroup.createServiceEnvelopeMessage`. Throws when either id is
    /// empty or longer than `lineageGroupIdMaxLen()`, or the two are equal.
    #[wasm_bindgen(js_name = setLineage)]
    pub fn set_lineage(
        &self,
        hlc: u64,
        successor_group_id: &[u8],
        root_conversation_id: &[u8],
    ) -> Result<bool, JsError> {
        self.with(|s| s.set_lineage(hlc, successor_group_id, root_conversation_id))?
            .map_err(js)
    }

    /// The lineage register held for a successor group, or `undefined` (the
    /// group is a root, or this device knows no lineage for it).
    #[wasm_bindgen(js_name = lineageRoot)]
    pub fn lineage_root(
        &self,
        successor_group_id: &[u8],
    ) -> Result<Option<LineageRegister>, JsError> {
        self.with(|s| {
            s.lineage_root(successor_group_id)
                .map(|inner| LineageRegister { inner })
        })
    }

    /// The conversation id a group belongs to: the root its lineage register
    /// names, or `groupId` itself when it holds none.
    #[wasm_bindgen(js_name = conversationIdOf)]
    pub fn conversation_id_of(&self, group_id: &[u8]) -> Result<Vec<u8>, JsError> {
        self.with(|s| s.conversation_id_of(group_id))
    }

    /// Every successor recorded for one conversation, ordered by `(stamp,
    /// successor id)`. The root itself is not listed.
    #[wasm_bindgen(js_name = lineageSuccessors)]
    pub fn lineage_successors(
        &self,
        root_conversation_id: &[u8],
    ) -> Result<Vec<LineageRegister>, JsError> {
        self.with(|s| {
            s.lineage_successors(root_conversation_id)
                .into_iter()
                .map(|inner| LineageRegister { inner })
                .collect()
        })
    }

    /// Every lineage register, ascending by successor id.
    #[wasm_bindgen(js_name = allLineage)]
    pub fn all_lineage(&self) -> Result<Vec<LineageRegister>, JsError> {
        self.with(|s| {
            s.all_lineage()
                .into_iter()
                .map(|inner| LineageRegister { inner })
                .collect()
        })
    }

    /// The envelope carrying the one register held for `successorGroupId`
    /// with its original stamp, or `undefined` when none is held: the live
    /// send after `setLineage` returned `true`.
    #[wasm_bindgen(js_name = lineageEnvelopeFor)]
    pub fn lineage_envelope_for(
        &self,
        successor_group_id: &[u8],
    ) -> Result<Option<Vec<u8>>, JsError> {
        self.with(|s| s.lineage_envelope_for(successor_group_id))
    }

    /// Every held lineage register with its original stamp, as service
    /// envelopes: the catch-up re-send beside `snapshotParts()` (lineage
    /// registers are NOT in the snapshot parts). Empty when none is held.
    #[wasm_bindgen(js_name = lineageEnvelopes)]
    pub fn lineage_envelopes(&self) -> Result<Vec<js_sys::Uint8Array>, JsError> {
        Ok(self
            .lineage_envelopes_inner()?
            .iter()
            .map(|e| js_sys::Uint8Array::from(e.as_slice()))
            .collect())
    }

    /// Decode a lineage envelope received in a DeviceGroup service envelope
    /// (route on `isLineageEnvelope`) and fold every register in with its
    /// original stamp. Fail closed: a malformed envelope changes nothing.
    /// Returns whether anything changed. Throws an `Error` whose message is
    /// `"<code>: <reason>"` with code `wrongDomain` / `unknownVersion` /
    /// `truncated` / `trailing` / `count` / `notCanonical` / `bounds`.
    #[wasm_bindgen(js_name = applyLineageEnvelope)]
    pub fn apply_lineage_envelope(&self, envelope: &[u8]) -> Result<bool, JsError> {
        self.with(|s| s.apply_lineage_envelope(envelope))?
            .map_err(|e| JsError::new(&format!("{}: {e}", e.code())))
    }

    /// Write the account's recovery email register (ADR-0075, amended
    /// 2026-10-13), stamped `hlc`: an address sets it, `undefined` / `null`
    /// (or a blank string) records that the account has none. The address
    /// is normalised here with the one rule (`normalizeRecoveryEmail`).
    ///
    /// An ordinary last-writer-wins write, NOT write-once: `true` when the
    /// register now holds this value, in which case send
    /// `recoveryEmailEnvelope()` to the account's other devices with
    /// `DeviceGroup.createServiceEnvelopeMessage`; `false` when what is held
    /// is newer or identical and nothing changed. A removal is kept with its
    /// stamp, so a stale device cannot bring the address back.
    ///
    /// The address is personal data: it stays on the account's devices and
    /// in the sealed backup, and must not be logged. Throws an `Error`
    /// whose message is `"<code>: <reason>"` (the codes of
    /// `normalizeRecoveryEmail`) when the address is refused.
    #[wasm_bindgen(js_name = setRecoveryEmail)]
    pub fn set_recovery_email(&self, hlc: u64, address: Option<String>) -> Result<bool, JsError> {
        let normalised =
            normalised_recovery_email(address.as_deref()).map_err(|m| JsError::new(&m))?;
        self.with(|s| s.set_recovery_email(hlc, normalised.as_deref()))?
            .map_err(js)
    }

    /// The account's recovery email address, or `undefined` when the
    /// register was never written or holds a removal.
    #[wasm_bindgen(js_name = recoveryEmail)]
    pub fn recovery_email(&self) -> Result<Option<String>, JsError> {
        self.with(|s| s.recovery_email())
    }

    /// The recovery email register as held, a removal included (`address`
    /// undefined, with its stamp); `undefined` when it was never written.
    #[wasm_bindgen(js_name = recoveryEmailRegister)]
    pub fn recovery_email_register(&self) -> Result<Option<RecoveryEmailRegister>, JsError> {
        self.with(|s| {
            s.recovery_email_register()
                .map(|inner| RecoveryEmailRegister { inner })
        })
    }

    /// The service envelope carrying the held register with its original
    /// stamp, a removal included: the live send after `setRecoveryEmail`
    /// returned `true`, and the catch-up re-send beside `snapshotParts()`
    /// and `lineageEnvelopes()` (the register is NOT in the snapshot parts).
    /// `undefined` when the register was never written.
    #[wasm_bindgen(js_name = recoveryEmailEnvelope)]
    pub fn recovery_email_envelope(&self) -> Result<Option<Vec<u8>>, JsError> {
        self.with(|s| s.recovery_email_envelope())
    }

    /// Decode a recovery-email envelope received in a DeviceGroup service
    /// envelope (route on `isRecoveryEmailEnvelope`) and fold the register
    /// in with its original stamp. Fail closed: a malformed envelope changes
    /// nothing. Returns whether anything changed. Throws an `Error` whose
    /// message is `"<code>: <reason>"` with code `wrongDomain` /
    /// `unknownVersion` / `truncated` / `trailing` / `notUtf8` / `bounds`.
    #[wasm_bindgen(js_name = applyRecoveryEmailEnvelope)]
    pub fn apply_recovery_email_envelope(&self, envelope: &[u8]) -> Result<bool, JsError> {
        self.with(|s| s.apply_recovery_email_envelope(envelope))?
            .map_err(|e| JsError::new(&format!("{}: {e}", e.code())))
    }

    /// Write the account's own-username register (ADR-0076 section 3),
    /// stamped `hlc`: a username sets it, `undefined` / `null` (or a blank
    /// string) records that the account has none. The username is
    /// normalised and checked here with the one rule (`validateUsername`),
    /// so `"@Bob"` may be passed and `"bob"` is stored.
    ///
    /// An ordinary last-writer-wins write, NOT write-once: `true` when the
    /// register now holds this value, in which case send
    /// `ownUsernameEnvelope()` to the account's other devices with
    /// `DeviceGroup.createServiceEnvelopeMessage`; `false` when what is held
    /// is newer or identical and nothing changed. A "none" is kept with its
    /// stamp, so a stale device cannot bring an old name back.
    ///
    /// Throws the same `Error` as `validateUsername` (own `usernameError`
    /// property `"empty"` / `"tooShort"` / `"tooLong"` /
    /// `"invalidCharacters"`) when the username is refused.
    #[wasm_bindgen(js_name = setOwnUsername)]
    pub fn set_own_username(&self, hlc: u64, username: Option<String>) -> Result<bool, JsValue> {
        let canonical = canonical_own_username(username.as_deref())
            .map_err(crate::discovery::username_error_value)?;
        self.with(|s| s.set_own_username(hlc, canonical.as_deref()))
            .map_err(JsValue::from)?
            .map_err(|e| JsValue::from(js(e)))
    }

    /// The account's own username, or `undefined` when the register was
    /// never written or holds "none".
    #[wasm_bindgen(js_name = ownUsername)]
    pub fn own_username(&self) -> Result<Option<String>, JsError> {
        self.with(|s| s.own_username())
    }

    /// The own-username register as held, a "none" included (`username`
    /// undefined, with its stamp); `undefined` when it was never written.
    #[wasm_bindgen(js_name = ownUsernameRegister)]
    pub fn own_username_register(&self) -> Result<Option<OwnUsernameRegister>, JsError> {
        self.with(|s| {
            s.own_username_register()
                .map(|inner| OwnUsernameRegister { inner })
        })
    }

    /// The service envelope carrying the held register with its original
    /// stamp, a "none" included: the live send after `setOwnUsername`
    /// returned `true`, and the catch-up re-send beside `snapshotParts()`,
    /// `lineageEnvelopes()` and `recoveryEmailEnvelope()` (the register is
    /// NOT in the snapshot parts). `undefined` when the register was never
    /// written.
    #[wasm_bindgen(js_name = ownUsernameEnvelope)]
    pub fn own_username_envelope(&self) -> Result<Option<Vec<u8>>, JsError> {
        self.with(|s| s.own_username_envelope())
    }

    /// Decode an own-username envelope received in a DeviceGroup service
    /// envelope (route on `isOwnUsernameEnvelope`) and fold the register in
    /// with its original stamp. Fail closed: a malformed envelope changes
    /// nothing. Returns whether anything changed. Throws an `Error` whose
    /// message is `"<code>: <reason>"` with code `wrongDomain` /
    /// `unknownVersion` / `truncated` / `trailing` / `notUtf8` / `bounds`.
    #[wasm_bindgen(js_name = applyOwnUsernameEnvelope)]
    pub fn apply_own_username_envelope(&self, envelope: &[u8]) -> Result<bool, JsError> {
        self.with(|s| s.apply_own_username_envelope(envelope))?
            .map_err(|e| JsError::new(&format!("{}: {e}", e.code())))
    }

    /// Write one of the account's invite links (ADR-0079 section 4) at
    /// `hlc`. `true` when written; then send `linkRecordEnvelopeFor(linkId)`
    /// in a DeviceGroup service envelope.
    #[wasm_bindgen(js_name = setLinkRecord)]
    pub fn set_link_record(&self, hlc: u64, record: &LinkRecord) -> Result<bool, JsError> {
        let value = record.inner.value.clone();
        self.with(|s| s.set_link_record(hlc, value))?
            .map_err(|e| JsError::new(&format!("bounds: {e}")))
    }

    /// The record held for a link id, or `undefined`.
    #[wasm_bindgen(js_name = linkRecord)]
    pub fn link_record(&self, link_id: &[u8]) -> Result<Option<LinkRecord>, JsError> {
        let Ok(id) = <[u8; 16]>::try_from(link_id) else {
            return Ok(None);
        };
        self.with(|s| s.link_record(&id).map(|inner| LinkRecord { inner }))
    }

    /// Every link record (revoked ones included), ascending by link id.
    #[wasm_bindgen(js_name = allLinkRecords)]
    pub fn all_link_records(&self) -> Result<Vec<LinkRecord>, JsError> {
        self.with(|s| {
            s.all_link_records()
                .into_iter()
                .map(|inner| LinkRecord { inner })
                .collect()
        })
    }

    /// Mark a link revoked (final). `true` when the register changed.
    #[wasm_bindgen(js_name = revokeLinkRecord)]
    pub fn revoke_link_record(&self, hlc: u64, link_id: &[u8]) -> Result<bool, JsError> {
        let id = <[u8; 16]>::try_from(link_id)
            .map_err(|_| JsError::new("bounds: link id must be 16 bytes"))?;
        self.with(|s| s.revoke_link_record(hlc, &id))?
            .map_err(|e| JsError::new(&format!("bounds: {e}")))
    }

    /// Count one request accepted through a link from `requesterKey` (the
    /// request's 32-byte `fromPubkey`). `true` when the register changed.
    #[wasm_bindgen(js_name = recordLinkUse)]
    pub fn record_link_use(
        &self,
        hlc: u64,
        link_id: &[u8],
        requester_key: &[u8],
    ) -> Result<bool, JsError> {
        let id = <[u8; 16]>::try_from(link_id)
            .map_err(|_| JsError::new("bounds: link id must be 16 bytes"))?;
        let key = <[u8; 32]>::try_from(requester_key)
            .map_err(|_| JsError::new("bounds: requester key must be 32 bytes"))?;
        self.with(|s| s.record_link_use(hlc, &id, &key))?
            .map_err(|e| JsError::new(&format!("bounds: {e}")))
    }

    /// The one rule for a contact request carrying a link answer (ADR-0079
    /// section 3): `"accept"` (run the accept path with no card, then
    /// `recordLinkUse`), `"answered"` (show nothing) or `"dead"` (show the
    /// card with the "no longer works" note). The secret is compared in
    /// constant time; an answer of the wrong width is `"dead"`.
    #[wasm_bindgen(js_name = linkAnswerVerdict)]
    pub fn link_answer_verdict(
        &self,
        link_id: &[u8],
        link_secret: &[u8],
        requester_key: &[u8],
        now_ms: u64,
    ) -> Result<String, JsError> {
        let (Ok(id), Ok(secret), Ok(key)) = (
            <[u8; 16]>::try_from(link_id),
            <[u8; 12]>::try_from(link_secret),
            <[u8; 32]>::try_from(requester_key),
        ) else {
            return Ok(core::LinkAnswerVerdict::Dead.code().to_string());
        };
        self.with(|s| {
            s.link_answer_verdict(&id, &secret, &key, now_ms)
                .code()
                .to_string()
        })
    }

    /// The service envelope carrying one held link record with its original
    /// stamp, or `undefined`.
    #[wasm_bindgen(js_name = linkRecordEnvelopeFor)]
    pub fn link_record_envelope_for(&self, link_id: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        let Ok(id) = <[u8; 16]>::try_from(link_id) else {
            return Ok(None);
        };
        self.with(|s| s.link_record_envelope_for(&id))
    }

    /// Every held link record as service envelopes: the catch-up re-send
    /// beside `snapshotParts()` (link records are NOT in the parts).
    #[wasm_bindgen(js_name = linkRecordEnvelopes)]
    pub fn link_record_envelopes(&self) -> Result<Vec<js_sys::Uint8Array>, JsError> {
        self.with(|s| {
            s.link_record_envelopes()
                .iter()
                .map(|e| js_sys::Uint8Array::from(e.as_slice()))
                .collect()
        })
    }

    /// Decode a link-record envelope (route on `isLinkRecordEnvelope`) and
    /// fold every register in. Fail closed. Returns whether anything
    /// changed. Throws `"<code>: <reason>"` with code `wrongDomain` /
    /// `unknownVersion` / `truncated` / `trailing` / `count` /
    /// `notCanonical` / `entry`.
    #[wasm_bindgen(js_name = applyLinkRecordEnvelope)]
    pub fn apply_link_record_envelope(&self, envelope: &[u8]) -> Result<bool, JsError> {
        self.with(|s| s.apply_link_record_envelope(envelope))?
            .map_err(|e| JsError::new(&format!("{}: {e}", e.code())))
    }

    /// The account's saved messages, most recently saved first (ADR-0065
    /// section 4). Removed ones are not listed.
    pub fn saved(&self) -> Result<Vec<SavedMessageView>, JsError> {
        self.with(|s| {
            s.saved()
                .into_iter()
                .map(|inner| SavedMessageView { inner })
                .collect()
        })
    }

    /// Whether this message is saved now (the row label and the saved
    /// mark). `false` for a message never saved, for a removed one, and for
    /// a conversation reference that is not well formed.
    #[wasm_bindgen(js_name = isSaved)]
    pub fn is_saved(&self, ref_kind: u8, ref_id: &[u8], message_id: u64) -> Result<bool, JsError> {
        let Ok(kind) = ConversationRefKind::from_wire(i64::from(ref_kind)) else {
            return Ok(false);
        };
        let conversation = ConversationRef {
            kind,
            id: ref_id.to_vec(),
        };
        self.with(|s| s.is_saved(&conversation, message_id))
    }

    /// Every blob id a saved message names, each once, in bytewise order,
    /// flat-concatenated (32 bytes each): what this device sends keeps for
    /// (ADR-0065 section 3).
    #[wasm_bindgen(js_name = savedBlobIds)]
    pub fn saved_blob_ids(&self) -> Result<Vec<u8>, JsError> {
        self.with(|s| s.saved_blob_ids().concat())
    }

    /// The greatest stamp held, or 0. Seed the HLC above it after a restore.
    #[wasm_bindgen(js_name = maxHlc)]
    pub fn max_hlc(&self) -> Result<u64, JsError> {
        self.with(|s| s.max_hlc())
    }

    /// The catch-up re-send (ADR-0058 section 5.1): the whole state, every
    /// entry with its original stamp, split so each encoded part is at most
    /// 256 KiB. Always at least one part. Throws when more than 64 parts
    /// would be needed.
    #[wasm_bindgen(js_name = snapshotParts)]
    pub fn snapshot_parts(&self) -> Result<Vec<SyncedSnapshotPart>, JsError> {
        let snap = self.with(|s| s.snapshot())?;
        let parts =
            lakko_mls::device_group_sync::split_state_snapshot(snap, core::SNAPSHOT_MAX_PART_BYTES)
                .map_err(js)?;
        Ok(parts
            .into_iter()
            .map(|inner| SyncedSnapshotPart { inner })
            .collect())
    }
}

impl Default for SyncedState {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether this device publishes the catch-up snapshot after the
/// DeviceGroup grew (ADR-0058 section 5.1): only the lowest leaf among the
/// members BEFORE the growth. Wait a random delay of up to 30 seconds first.
#[wasm_bindgen(js_name = shouldSendSnapshot)]
pub fn should_send_snapshot(own_leaf: u32, prior_member_leaves: Vec<u32>) -> bool {
    core::should_send_snapshot(own_leaf, &prior_member_leaves)
}

/// `true` when `bytes` open with the lineage envelope domain
/// (`lakko/sync/lineage/v1`): the routing question for a DeviceGroup service
/// envelope, asked before the `lakko-contacts` dispatcher. Says nothing
/// about validity; `SyncedState.applyLineageEnvelope` decides that.
#[wasm_bindgen(js_name = isLineageEnvelope)]
#[must_use]
pub fn is_lineage_envelope(bytes: &[u8]) -> bool {
    core::is_lineage_envelope(bytes)
}

/// Longest lineage envelope `SyncedState.lineageEnvelopes()` produces; below
/// `deviceGroupServiceEnvelopeMaxLen()`.
#[wasm_bindgen(js_name = lineageEnvelopeMaxLen)]
#[must_use]
pub fn lineage_envelope_max_len() -> u32 {
    u32::try_from(core::LINEAGE_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Longest group id a lineage register accepts (each of the two): 64.
#[wasm_bindgen(js_name = lineageGroupIdMaxLen)]
#[must_use]
pub fn lineage_group_id_max_len() -> u32 {
    u32::try_from(core::LINEAGE_GROUP_ID_MAX_LEN).unwrap_or(u32::MAX)
}

/// `true` when `bytes` open with the recovery-email envelope domain
/// (`lakko/sync/recovery-email/v1`): the routing question for a DeviceGroup
/// service envelope, asked before the `lakko-contacts` dispatcher. Says
/// nothing about validity; `SyncedState.applyRecoveryEmailEnvelope` decides
/// that.
#[wasm_bindgen(js_name = isRecoveryEmailEnvelope)]
#[must_use]
pub fn is_recovery_email_envelope(bytes: &[u8]) -> bool {
    core::is_recovery_email_envelope(bytes)
}

/// Longest recovery-email envelope `SyncedState.recoveryEmailEnvelope()`
/// produces (293); below `deviceGroupServiceEnvelopeMaxLen()`.
#[wasm_bindgen(js_name = recoveryEmailEnvelopeMaxLen)]
#[must_use]
pub fn recovery_email_envelope_max_len() -> u32 {
    u32::try_from(core::RECOVERY_EMAIL_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// `true` when `bytes` open with the own-username envelope domain
/// (`lakko/sync/username/v1`): the routing question for a DeviceGroup
/// service envelope, asked before the `lakko-contacts` dispatcher. Says
/// nothing about validity; `SyncedState.applyOwnUsernameEnvelope` decides
/// that.
#[wasm_bindgen(js_name = isOwnUsernameEnvelope)]
#[must_use]
pub fn is_own_username_envelope(bytes: &[u8]) -> bool {
    core::is_own_username_envelope(bytes)
}

/// `true` when `bytes` open with the link-record envelope domain
/// (`lakko/sync/link-record/v1`, ADR-0079): the routing question for a
/// DeviceGroup service envelope.
#[wasm_bindgen(js_name = isLinkRecordEnvelope)]
#[must_use]
pub fn is_link_record_envelope(bytes: &[u8]) -> bool {
    core::is_link_record_envelope(bytes)
}

/// Longest own-username envelope `SyncedState.ownUsernameEnvelope()`
/// produces (65); below `deviceGroupServiceEnvelopeMaxLen()`.
#[wasm_bindgen(js_name = ownUsernameEnvelopeMaxLen)]
#[must_use]
pub fn own_username_envelope_max_len() -> u32 {
    u32::try_from(core::OWN_USERNAME_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Whether `name` is an acceptable folder name as it stands (1..=24
/// characters, at most 96 bytes, the display-name rule, trimmed).
#[wasm_bindgen(js_name = isValidFolderName)]
pub fn is_valid_folder_name(name: &str) -> bool {
    core::check_folder_name(name).is_ok()
}

/// A fresh random 16-byte folder id, minted once when a folder is created.
#[wasm_bindgen(js_name = mintFolderId)]
pub fn mint_folder_id() -> Vec<u8> {
    let mut id = [0u8; core::FOLDER_ID_LEN];
    lakko_crypto::random::fill(&mut id);
    id.to_vec()
}

/// How many saved messages a client lets one account hold: refuse a new
/// Save once `SyncedState.saved()` lists this many. A client-side limit,
/// chosen; NOT a decode bound (two devices that each saved while offline
/// can exceed it together, and that state still decodes and folds).
#[wasm_bindgen(js_name = savedMaxClient)]
#[must_use]
pub fn saved_max_client() -> u32 {
    u32::try_from(core::SAVED_MAX_CLIENT).unwrap_or(u32::MAX)
}

/// How many saved registers a client puts in one change set. A larger
/// selection (Select mode's Save) goes out as several change sets, so one
/// DeviceGroup message stays well under what the Delivery Service accepts.
/// A client-side sending rule, chosen; NOT a decode bound.
#[wasm_bindgen(js_name = savedMaxPerChangeSet)]
#[must_use]
pub fn saved_max_per_change_set() -> u32 {
    u32::try_from(core::SAVED_MAX_PER_CHANGE_SET).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Success paths only: a `JsError` cannot be built off wasm32, and the
    /// refusals are pinned in `lakko-sync` and on the UniFFI surface.
    #[test]
    fn lineage_is_write_once_travels_by_envelope_and_persists() {
        let a = SyncedState::new();
        assert!(a.set_lineage(5, &[1; 32], &[9; 32]).unwrap_or(false));
        assert!(!a.set_lineage(8, &[1; 32], &[7; 32]).unwrap_or(true));
        assert!(a.set_lineage(6, &[2; 32], &[9; 32]).unwrap_or(false));
        let held = a.lineage_root(&[1; 32]).ok().flatten();
        assert_eq!(held.as_ref().map(LineageRegister::hlc), Some(5));
        assert_eq!(held.map(|h| h.root_conversation_id()), Some(vec![9; 32]));
        assert_eq!(
            a.conversation_id_of(&[2; 32]).unwrap_or_default(),
            vec![9; 32]
        );
        assert_eq!(
            a.conversation_id_of(&[9; 32]).unwrap_or_default(),
            vec![9; 32]
        );
        assert_eq!(a.lineage_successors(&[9; 32]).unwrap_or_default().len(), 2);

        let b = SyncedState::new();
        let live = a
            .lineage_envelope_for(&[1; 32])
            .ok()
            .flatten()
            .unwrap_or_default();
        assert!(is_lineage_envelope(&live));
        assert!(b.apply_lineage_envelope(&live).unwrap_or(false));
        for envelope in a.lineage_envelopes_inner().unwrap_or_default() {
            assert!(envelope.len() <= lineage_envelope_max_len() as usize);
            let _ = b.apply_lineage_envelope(&envelope);
        }
        assert_eq!(
            b.all_lineage().unwrap_or_default(),
            a.all_lineage().unwrap_or_default()
        );
        assert_eq!(b.all_lineage().unwrap_or_default().len(), 2);

        let parts = a.snapshot_parts().unwrap_or_default();
        assert_eq!(parts[0].entry_count(), 0);
        let bytes = a.to_bytes().unwrap_or_default();
        let back = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert_eq!(back.all_lineage().unwrap_or_default().len(), 2);
        assert!(lineage_envelope_max_len() <= crate::mls::device_group_service_envelope_max_len());
        assert_eq!(lineage_group_id_max_len(), 64);
    }

    /// Success paths, plus the refusal of the pure normalising helper (a
    /// `JsError` cannot be built off wasm32).
    #[test]
    fn the_recovery_email_is_set_removed_carried_and_persisted() {
        let a = SyncedState::new();
        assert_eq!(a.recovery_email().ok().flatten(), None);
        assert!(a.recovery_email_envelope().ok().flatten().is_none());
        assert!(a
            .set_recovery_email(5, Some("  Alice@Example.COM ".into()))
            .unwrap_or(false));
        assert_eq!(
            a.recovery_email().ok().flatten().as_deref(),
            Some("alice@example.com")
        );
        assert!(!a
            .set_recovery_email(5, Some("alice@example.com".into()))
            .unwrap_or(true));
        assert!(!a
            .set_recovery_email(4, Some("old@example.com".into()))
            .unwrap_or(true));

        let b = SyncedState::new();
        let live = a
            .recovery_email_envelope()
            .ok()
            .flatten()
            .unwrap_or_default();
        assert!(is_recovery_email_envelope(&live));
        assert!(!is_lineage_envelope(&live));
        assert!(live.len() <= recovery_email_envelope_max_len() as usize);
        assert!(b.apply_recovery_email_envelope(&live).unwrap_or(false));
        assert!(!b.apply_recovery_email_envelope(&live).unwrap_or(true));
        let held = b.recovery_email_register().ok().flatten();
        assert_eq!(held.as_ref().map(RecoveryEmailRegister::hlc), Some(5));
        assert_eq!(
            held.and_then(|h| h.address()).as_deref(),
            Some("alice@example.com")
        );

        // The snapshot parts do not carry it; persistence does.
        let parts = a.snapshot_parts().unwrap_or_default();
        assert_eq!(parts[0].entry_count(), 0);
        let bytes = a.to_bytes().unwrap_or_default();
        let back = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert_eq!(
            back.recovery_email().ok().flatten().as_deref(),
            Some("alice@example.com")
        );

        // A removal is kept, beats the older address, and a stale envelope
        // cannot undo it; a newer address beats the removal.
        assert!(a.set_recovery_email(7, None).unwrap_or(false));
        assert_eq!(a.recovery_email().ok().flatten(), None);
        let removed = a.recovery_email_register().ok().flatten();
        assert_eq!(removed.as_ref().map(RecoveryEmailRegister::hlc), Some(7));
        assert_eq!(removed.and_then(|r| r.address()), None);
        assert!(!a.apply_recovery_email_envelope(&live).unwrap_or(true));
        let removal = a
            .recovery_email_envelope()
            .ok()
            .flatten()
            .unwrap_or_default();
        assert!(b.apply_recovery_email_envelope(&removal).unwrap_or(false));
        assert_eq!(b.recovery_email().ok().flatten(), None);
        assert!(a
            .set_recovery_email(8, Some("new@example.com".into()))
            .unwrap_or(false));
        assert_eq!(
            a.recovery_email().ok().flatten().as_deref(),
            Some("new@example.com")
        );

        assert_eq!(normalised_recovery_email(None), Ok(None));
        assert_eq!(normalised_recovery_email(Some("  ")), Ok(None));
        assert_eq!(
            normalised_recovery_email(Some(" A@B ")),
            Ok(Some("a@b".to_owned()))
        );
        assert!(normalised_recovery_email(Some("no-at-sign"))
            .is_err_and(|m| m.starts_with("not-one-at-sign: ")));
        assert!(
            recovery_email_envelope_max_len()
                <= crate::mls::device_group_service_envelope_max_len()
        );
    }

    /// Success paths, plus the refusal of the pure canonicalising helper (a
    /// JS error value cannot be built off wasm32).
    #[test]
    fn the_own_username_is_set_cleared_carried_and_persisted() {
        use lakko_discovery_client::UsernameError;
        let a = SyncedState::new();
        assert_eq!(a.own_username().ok().flatten(), None);
        assert!(a.own_username_envelope().ok().flatten().is_none());
        assert!(a
            .set_own_username(5, Some(" @Bob_42 ".into()))
            .unwrap_or(false));
        assert_eq!(a.own_username().ok().flatten().as_deref(), Some("bob_42"));
        assert!(!a.set_own_username(5, Some("bob_42".into())).unwrap_or(true));
        assert!(!a.set_own_username(4, Some("older".into())).unwrap_or(true));

        let b = SyncedState::new();
        let live = a.own_username_envelope().ok().flatten().unwrap_or_default();
        assert!(is_own_username_envelope(&live));
        assert!(!is_recovery_email_envelope(&live));
        assert!(!is_lineage_envelope(&live));
        assert!(live.len() <= own_username_envelope_max_len() as usize);
        assert!(b.apply_own_username_envelope(&live).unwrap_or(false));
        assert!(!b.apply_own_username_envelope(&live).unwrap_or(true));
        let held = b.own_username_register().ok().flatten();
        assert_eq!(held.as_ref().map(OwnUsernameRegister::hlc), Some(5));
        assert_eq!(held.and_then(|h| h.username()).as_deref(), Some("bob_42"));

        let parts = a.snapshot_parts().unwrap_or_default();
        assert_eq!(parts[0].entry_count(), 0);
        let bytes = a.to_bytes().unwrap_or_default();
        let back = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert_eq!(
            back.own_username().ok().flatten().as_deref(),
            Some("bob_42")
        );

        // A rename, then "none", each beat what is older; a stale envelope
        // cannot undo either; a newer name beats the "none".
        assert!(a
            .set_own_username(6, Some("robert".into()))
            .unwrap_or(false));
        assert!(!a.apply_own_username_envelope(&live).unwrap_or(true));
        assert!(a.set_own_username(7, None).unwrap_or(false));
        assert_eq!(a.own_username().ok().flatten(), None);
        let cleared = a.own_username_register().ok().flatten();
        assert_eq!(cleared.as_ref().map(OwnUsernameRegister::hlc), Some(7));
        assert_eq!(cleared.and_then(|r| r.username()), None);
        assert!(!a.apply_own_username_envelope(&live).unwrap_or(true));
        let none = a.own_username_envelope().ok().flatten().unwrap_or_default();
        assert!(b.apply_own_username_envelope(&none).unwrap_or(false));
        assert_eq!(b.own_username().ok().flatten(), None);
        assert!(a.set_own_username(8, Some("bobby".into())).unwrap_or(false));
        assert_eq!(a.own_username().ok().flatten().as_deref(), Some("bobby"));

        assert_eq!(canonical_own_username(None), Ok(None));
        assert_eq!(canonical_own_username(Some("  ")), Ok(None));
        assert_eq!(
            canonical_own_username(Some("@Bob")),
            Ok(Some("bob".to_owned()))
        );
        assert!(matches!(
            canonical_own_username(Some("ab")),
            Err(UsernameError::TooShort { .. })
        ));
        assert_eq!(
            canonical_own_username(Some("bo b")),
            Err(UsernameError::InvalidCharacters)
        );
        assert!(
            own_username_envelope_max_len() <= crate::mls::device_group_service_envelope_max_len()
        );
    }

    #[test]
    fn a_local_edit_folds_persists_and_snapshots() {
        let mut c = FolderChangeSet::new();
        c.add_meta(5, &[1; 16], "Work".into(), 0b010, 0b100)
            .unwrap_or_else(|_| unreachable!());
        c.add_member(6, &[1; 16], 3, &[7; 32], 1)
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(c.register_count(), 2);
        let s = SyncedState::new();
        assert!(s.apply_folder_changes(&c).unwrap_or(false));
        let f = s.folders().unwrap_or_default();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name(), "Work");
        assert_eq!(f[0].included()[0].kind(), 3);

        let bytes = s.to_bytes().unwrap_or_default();
        let back = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert_eq!(back.to_bytes().unwrap_or_default(), bytes);

        let parts = s.snapshot_parts().unwrap_or_default();
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].part(), parts[0].parts()), (0, 1));
        assert!(should_send_snapshot(0, vec![0, 2]));
        assert!(is_valid_folder_name("Work"));
    }

    const CHAT: [u8; 32] = [9; 32];

    fn blobs(ids: &[u8]) -> Vec<u8> {
        ids.iter().flat_map(|b| [*b; 32]).collect()
    }

    fn save(hlc: u64, message_id: u64, saved_at_ms: u64, ids: &[u8]) -> FolderChangeSet {
        let mut c = FolderChangeSet::new();
        c.add_saved(
            hlc,
            2,
            &CHAT,
            message_id,
            saved_at_ms,
            "Ada".into(),
            format!("message {message_id}"),
            if ids.is_empty() { "" } else { "photo" }.into(),
            &blobs(ids),
        )
        .unwrap_or_else(|_| unreachable!());
        c
    }

    fn remove(hlc: u64, message_id: u64) -> FolderChangeSet {
        let mut c = FolderChangeSet::new();
        c.add_saved_removed(hlc, 2, &CHAT, message_id)
            .unwrap_or_else(|_| unreachable!());
        c
    }

    fn apply(s: &SyncedState, c: &FolderChangeSet) -> bool {
        s.apply_folder_changes(c).unwrap_or_else(|_| unreachable!())
    }

    fn is_saved(s: &SyncedState, message_id: u64) -> bool {
        s.is_saved(2, &CHAT, message_id)
            .unwrap_or_else(|_| unreachable!())
    }

    fn listed(s: &SyncedState) -> Vec<u64> {
        s.saved()
            .unwrap_or_default()
            .iter()
            .map(SavedMessageView::message_id)
            .collect()
    }

    #[test]
    fn save_list_remove_and_save_again() {
        let s = SyncedState::new();
        assert!(!is_saved(&s, 7));
        let c = save(10, 7, 100, &[1, 2]);
        assert_eq!(c.register_count(), 1);
        assert!(apply(&s, &c));
        // The same write again changes nothing.
        assert!(!apply(&s, &c));
        assert!(apply(&s, &save(11, 8, 200, &[2, 3])));
        assert!(is_saved(&s, 7));
        // Another kind or another conversation is another register; a kind
        // that is not one at all is simply not saved.
        assert!(!s.is_saved(3, &CHAT, 7).unwrap_or(true));
        assert!(!s.is_saved(2, &[8; 32], 7).unwrap_or(true));
        assert!(!s.is_saved(0, &CHAT, 7).unwrap_or(true));

        // Most recently saved first.
        assert_eq!(listed(&s), vec![8, 7]);
        let list = s.saved().unwrap_or_default();
        let row = &list[1];
        assert_eq!((row.hlc(), row.saved_at_ms()), (10, 100));
        assert_eq!(row.author(), "Ada");
        assert_eq!(row.excerpt(), "message 7");
        assert_eq!(row.attachment_kind(), "photo");
        assert_eq!((row.ref_kind(), row.ref_id()), (2, CHAT.to_vec()));
        assert_eq!(row.conversation().kind(), 2);
        assert_eq!(row.blob_ids(), blobs(&[1, 2]));
        // Each blob id once, across messages, flat-concatenated.
        assert_eq!(s.saved_blob_ids().unwrap_or_default(), blobs(&[1, 2, 3]));

        // Remove: gone from every read, kept as a tombstone, and an older
        // save cannot bring it back.
        assert!(apply(&s, &remove(12, 7)));
        assert!(!is_saved(&s, 7));
        assert_eq!(listed(&s), vec![8]);
        assert_eq!(s.saved_blob_ids().unwrap_or_default(), blobs(&[2, 3]));
        assert!(!apply(&s, &c));
        assert!(!is_saved(&s, 7));

        // Saved again, with a greater stamp.
        assert!(apply(&s, &save(13, 7, 300, &[])));
        assert!(is_saved(&s, 7));
        assert_eq!(listed(&s), vec![7, 8]);
        assert_eq!(s.max_hlc().unwrap_or(0), 13);
    }

    #[test]
    fn saved_messages_persist_and_reach_a_late_device() {
        use lakko_proto::mls_app::devicegroup::v1 as pb;
        use prost::Message as _;

        let a = SyncedState::new();
        apply(&a, &save(10, 7, 100, &[1]));
        apply(&a, &save(11, 8, 200, &[]));
        apply(&a, &remove(12, 8));

        // Persistence (also what the backup payload carries), and a merge.
        let bytes = a.to_bytes().unwrap_or_default();
        let restored = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert_eq!(restored.to_bytes().unwrap_or_default(), bytes);
        assert_eq!(listed(&restored), vec![7]);
        let fresh = SyncedState::new();
        apply(&fresh, &save(20, 9, 400, &[5]));
        assert!(fresh.merge_from(&restored).unwrap_or(false));
        assert_eq!(listed(&fresh), vec![9, 7]);

        // The snapshot carries the saved registers, tombstone included.
        let parts = a.snapshot_parts().unwrap_or_default();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].entry_count(), 2);

        // What another device sent, off the wire: a save and a removal,
        // each with its own stamp.
        let conversation = Some(pb::ConversationRef {
            kind: 2,
            id: CHAT.to_vec(),
        });
        let wire = pb::DeviceGroupPayload {
            hlc: 1,
            sender_leaf: 2,
            body: Some(pb::device_group_payload::Body::FolderChanges(
                pb::FolderChanges {
                    saved: vec![
                        pb::SavedMessage {
                            hlc: 50,
                            conversation: conversation.clone(),
                            message_id: 21,
                            state: 1,
                            saved_at_ms: 900,
                            author: "Ada".into(),
                            excerpt: "hello".into(),
                            attachment_kind: "voice".into(),
                            blob_ids: vec![vec![4; 32]],
                        },
                        pb::SavedMessage {
                            hlc: 51,
                            conversation,
                            message_id: 7,
                            state: 2,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            )),
        }
        .encode_to_vec();
        let Ok(parsed) = crate::mls::parse_devicegroup_payload(&wire) else {
            unreachable!()
        };
        assert_eq!(parsed.kind(), "folderChanges");
        let late = SyncedState::from_bytes(&bytes).unwrap_or_default();
        assert!(late.apply_payload(&parsed).unwrap_or(false));
        assert_eq!(listed(&late), vec![21]);
        assert!(!is_saved(&late, 7));
        assert_eq!(late.saved().unwrap_or_default()[0].hlc(), 50);
        assert_eq!(late.saved_blob_ids().unwrap_or_default(), blobs(&[4]));
    }

    #[test]
    fn composing_a_save_fits_the_text_and_refuses_what_it_cannot_repair() {
        use lakko_sync::synced::BoundsError as B;
        let long = "😀".repeat(300);
        let ok = saved_present(
            1,
            2,
            &CHAT,
            7,
            5,
            long.clone(),
            long.clone(),
            "file".into(),
            &blobs(&[1, 1, 2]),
        );
        let Ok(reg) = ok else { unreachable!() };
        assert!(matches!(
            &reg.value.value,
            core::SavedValue::Present { snapshot, blob_ids, .. }
                if snapshot.excerpt.len() == 512
                    && snapshot.author.len() == 252
                    && long.starts_with(snapshot.excerpt.as_str())
                    && *blob_ids == vec![[1u8; 32], [2; 32]]
        ));

        let present = |kind: u8, id: &[u8], attachment_kind: &str, blob_ids: &[u8]| {
            saved_present(
                1,
                kind,
                id,
                7,
                5,
                String::new(),
                String::new(),
                attachment_kind.into(),
                blob_ids,
            )
            .map(|_| ())
        };
        assert_eq!(present(2, &CHAT, "", &[]), Ok(()));
        assert_eq!(present(2, &CHAT, "", &[1; 31]), Err(B::BlobIdLength));
        assert_eq!(present(2, &CHAT, "", &[1; 33]), Err(B::BlobIdLength));
        assert_eq!(
            present(2, &CHAT, &"k".repeat(33), &[]),
            Err(B::SavedAttachmentKindTooLong)
        );
        let too_many: Vec<u8> = (0..129u8).flat_map(|i| [i; 32]).collect();
        assert_eq!(present(2, &CHAT, "", &too_many), Err(B::TooManyBlobIds));
        let at_limit: Vec<u8> = (0..128u8).flat_map(|i| [i; 32]).collect();
        assert_eq!(present(2, &CHAT, "", &at_limit), Ok(()));
        assert_eq!(present(0, &CHAT, "", &[]), Err(B::RefKindUnspecified));
        assert_eq!(present(2, &[], "", &[]), Err(B::RefIdLength));
        assert_eq!(present(2, &[0; 65], "", &[]), Err(B::RefIdLength));
        assert_eq!(
            saved_removed(1, 9, &CHAT, 7).map(|_| ()),
            Err(B::RefKindUnspecified)
        );
        assert_eq!(saved_removed(1, 2, &[], 7).map(|_| ()), Err(B::RefIdLength));
        assert_eq!(saved_removed(1, 1, &CHAT, 7).map(|_| ()), Ok(()));
    }

    #[test]
    fn the_client_limits_are_the_core_constants() {
        assert_eq!(saved_max_client(), 2000);
        assert_eq!(saved_max_per_change_set(), 32);
        assert_eq!(saved_max_client() as usize, core::SAVED_MAX_CLIENT);
        assert_eq!(
            saved_max_per_change_set() as usize,
            core::SAVED_MAX_PER_CHANGE_SET
        );
    }
}
