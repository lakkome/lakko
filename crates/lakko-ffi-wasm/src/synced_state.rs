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

    /// Number of registers carried.
    #[wasm_bindgen(getter, js_name = registerCount)]
    pub fn register_count(&self) -> u32 {
        u32::try_from(self.inner.register_count()).unwrap_or(u32::MAX)
    }

    /// Throw when the change set breaks an ADR-0058 section 4 bound (name,
    /// bitfields, pin / order / register caps).
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

    /// Fold a local folder edit in (the change set the client then sends).
    /// Throws when it breaks a bound.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
