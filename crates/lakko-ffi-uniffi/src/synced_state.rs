// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's synced state (ADR-0058) — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/synced_state.rs` over the same
//! [`lakko_sync::SyncedState`] fold. The fold is written once in Rust so the
//! three clients cannot disagree about which write wins (the ADR-0044
//! argument); a client holds one [`SyncedState`], feeds it every parsed
//! DeviceGroup payload and every local write, persists [`SyncedState::to_bytes`],
//! and reads folders, flags, cursors and drafts back from it.
//!
//! A folder id is 16 bytes; a conversation reference is `(kind, id)` with the
//! MLS group id for a one-to-one or a group and the channel id for a channel.
//!
//! ## Saved messages (ADR-0065 section 4)
//!
//! A saved message is one more register of the same state, carried in
//! [`FolderChangeSet::saved`]. To save or remove:
//!
//! 1. build the register with [`compose_saved_message`] /
//!    [`compose_saved_removal`] at a fresh HLC stamp;
//! 2. put it in a [`FolderChangeSet`] (`saved: [register]`, at most
//!    [`saved_max_per_change_set`] per change set);
//! 3. fold it locally with [`SyncedState::apply_folder_changes`] and send
//!    the same change set as `DeviceGroupBody::FolderChanges`.
//!
//! A received one arrives through [`SyncedState::apply_payload`] like any
//! folder edit, and a late device gets the list in the snapshot parts. Read
//! with [`SyncedState::saved`], [`SyncedState::is_saved`] and
//! [`SyncedState::saved_blob_ids`].

use std::sync::{Arc, Mutex};

use lakko_sync::synced::{
    self as core, ConversationRef, ConversationRefKind, FolderChanges, MemberState, Stamped,
};

use crate::{FfiError, ParsedDeviceGroupPayload};

/// Which kind of conversation a folder reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FolderConversationKind {
    /// A one-to-one conversation (MLS group id).
    OneToOne,
    /// A group (MLS group id).
    Group,
    /// A channel (channel id).
    Channel,
}

impl From<FolderConversationKind> for ConversationRefKind {
    fn from(k: FolderConversationKind) -> Self {
        match k {
            FolderConversationKind::OneToOne => Self::OneToOne,
            FolderConversationKind::Group => Self::Group,
            FolderConversationKind::Channel => Self::Channel,
        }
    }
}

impl From<ConversationRefKind> for FolderConversationKind {
    fn from(k: ConversationRefKind) -> Self {
        match k {
            ConversationRefKind::OneToOne => Self::OneToOne,
            ConversationRefKind::Group => Self::Group,
            ConversationRefKind::Channel => Self::Channel,
        }
    }
}

/// A conversation a folder names.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderConversationRef {
    /// Which kind.
    pub kind: FolderConversationKind,
    /// MLS group id, or channel id. 1..=64 bytes.
    pub id: Vec<u8>,
}

impl From<FolderConversationRef> for ConversationRef {
    fn from(r: FolderConversationRef) -> Self {
        Self {
            kind: r.kind.into(),
            id: r.id,
        }
    }
}

impl From<ConversationRef> for FolderConversationRef {
    fn from(r: ConversationRef) -> Self {
        Self {
            kind: r.kind.into(),
            id: r.id,
        }
    }
}

/// A conversation's membership in one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FolderMemberState {
    /// Neither added nor excluded by hand. Write this to remove a chat from
    /// a folder's hand-added or hand-excluded set (never delete).
    None,
    /// Added by hand.
    Included,
    /// Excluded by hand.
    Excluded,
}

impl From<FolderMemberState> for MemberState {
    fn from(s: FolderMemberState) -> Self {
        match s {
            FolderMemberState::None => Self::None,
            FolderMemberState::Included => Self::Included,
            FolderMemberState::Excluded => Self::Excluded,
        }
    }
}

/// A folder's name and toggles, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderMetaRegister {
    /// HLC stamp of this register.
    pub hlc: u64,
    /// 16 bytes.
    pub folder_id: Vec<u8>,
    /// 1..=24 characters, the display-name rule.
    pub name: String,
    /// bit0 one-to-one, bit1 groups, bit2 channels.
    pub include_kinds: u32,
    /// bit0 muted, bit1 read, bit2 archived.
    pub exclude_flags: u32,
}

/// One conversation's membership register in one folder, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderMemberRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// 16 bytes.
    pub folder_id: Vec<u8>,
    /// The conversation.
    pub conversation: FolderConversationRef,
    /// Its state.
    pub state: FolderMemberState,
}

/// A folder's pin list (at most 10), as one stamped value.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderPinsRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// 16 bytes.
    pub folder_id: Vec<u8>,
    /// Pinned conversations, top first.
    pub pinned: Vec<FolderConversationRef>,
}

/// A folder deletion, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderRemovedRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// 16 bytes.
    pub folder_id: Vec<u8>,
}

/// The folder order (at most 256 ids), as one stamped value.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderOrderRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// Folder ids (16 bytes each), first tab first.
    pub folder_ids: Vec<Vec<u8>>,
}

/// A live folder edit: only the registers that changed, each with its own
/// stamp. At most 1024 registers.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FolderChangeSet {
    /// Meta registers.
    pub metas: Vec<FolderMetaRegister>,
    /// Member registers.
    pub members: Vec<FolderMemberRegister>,
    /// Pin-list registers.
    pub pins: Vec<FolderPinsRegister>,
    /// Tombstones.
    pub removed: Vec<FolderRemovedRegister>,
    /// The order, when it changed.
    pub order: Option<FolderOrderRegister>,
    /// Saved-message registers (ADR-0065 section 4): saves and removals.
    /// Defaults to empty, so a folder-only change set is built as before.
    #[uniffi(default = [])]
    pub saved: Vec<SavedMessageRegister>,
}

/// What a saved-message register holds.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SavedMessageValue {
    /// The message is saved.
    Present {
        /// Wall-clock millis of the save. Orders the saved list.
        saved_at_ms: u64,
        /// The author's display name as shown when saved. At most 255 bytes.
        author: String,
        /// The start of the message text. At most 512 bytes.
        excerpt: String,
        /// A short token for the kind of attachment; empty when the message
        /// has none. At most 32 bytes.
        attachment_kind: String,
        /// Blob ids (32 bytes each) of the message's files and thumbnails.
        /// At most 128, none repeated.
        blob_ids: Vec<Vec<u8>>,
    },
    /// The message was removed from saved (the tombstone).
    Removed,
}

/// One saved-message register, stamped (ADR-0065 section 4). Build it with
/// [`compose_saved_message`] / [`compose_saved_removal`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SavedMessageRegister {
    /// HLC stamp of this register.
    pub hlc: u64,
    /// The conversation the message is in.
    pub conversation: FolderConversationRef,
    /// The message's Delivery Service journal id.
    pub message_id: u64,
    /// Saved, or removed.
    pub value: SavedMessageValue,
}

/// One saved message as the Saved screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SavedMessageView {
    /// HLC stamp the save was written at.
    pub hlc: u64,
    /// The conversation the message is in.
    pub conversation: FolderConversationRef,
    /// The message's Delivery Service journal id.
    pub message_id: u64,
    /// Wall-clock millis of the save.
    pub saved_at_ms: u64,
    /// The author's display name as shown when saved.
    pub author: String,
    /// The start of the message text.
    pub excerpt: String,
    /// The attachment kind token; empty when the message has none.
    pub attachment_kind: String,
    /// Blob ids (32 bytes each) of the message's files and thumbnails.
    pub blob_ids: Vec<Vec<u8>>,
}

fn synced_err(e: core::BoundsError) -> FfiError {
    FfiError::SyncedState(e.to_string())
}

impl SavedMessageRegister {
    /// Convert and bounds-check. Nothing is cut or repaired here: a
    /// register handed over directly must already be within its bounds.
    fn into_core(self) -> Result<Stamped<core::SavedMessage>, FfiError> {
        let value = match self.value {
            SavedMessageValue::Removed => core::SavedValue::Removed,
            SavedMessageValue::Present {
                saved_at_ms,
                author,
                excerpt,
                attachment_kind,
                blob_ids,
            } => {
                if blob_ids.len() > core::SAVED_BLOB_IDS_MAX {
                    return Err(synced_err(core::BoundsError::TooManyBlobIds));
                }
                core::SavedValue::Present {
                    saved_at_ms,
                    snapshot: core::SavedSnapshot {
                        author,
                        excerpt,
                        attachment_kind,
                    },
                    blob_ids: blob_ids
                        .iter()
                        .map(|b| core::blob_id_from_slice(b).map_err(synced_err))
                        .collect::<Result<Vec<_>, _>>()?,
                }
            },
        };
        let saved = core::SavedMessage {
            conversation: self.conversation.into(),
            message_id: self.message_id,
            value,
        };
        saved.validate().map_err(synced_err)?;
        Ok(Stamped {
            hlc: self.hlc,
            value: saved,
        })
    }

    fn from_core(s: Stamped<core::SavedMessage>) -> Self {
        Self {
            hlc: s.hlc,
            conversation: s.value.conversation.into(),
            message_id: s.value.message_id,
            value: match s.value.value {
                core::SavedValue::Removed => SavedMessageValue::Removed,
                core::SavedValue::Present {
                    saved_at_ms,
                    snapshot,
                    blob_ids,
                } => SavedMessageValue::Present {
                    saved_at_ms,
                    author: snapshot.author,
                    excerpt: snapshot.excerpt,
                    attachment_kind: snapshot.attachment_kind,
                    blob_ids: blob_ids.iter().map(|b| b.to_vec()).collect(),
                },
            },
        }
    }
}

fn fid(bytes: &[u8]) -> Result<core::FolderId, FfiError> {
    core::folder_id_from_slice(bytes).map_err(|e| FfiError::SyncedState(e.to_string()))
}

impl FolderChangeSet {
    /// Convert and bounds-check (ADR-0058 section 4).
    pub(crate) fn into_core(self) -> Result<FolderChanges, FfiError> {
        let mut c = FolderChanges::default();
        for m in self.metas {
            c.metas.push(Stamped {
                hlc: m.hlc,
                value: core::FolderMeta {
                    folder_id: fid(&m.folder_id)?,
                    name: m.name,
                    include_kinds: m.include_kinds,
                    exclude_flags: m.exclude_flags,
                },
            });
        }
        for m in self.members {
            c.members.push(Stamped {
                hlc: m.hlc,
                value: core::FolderMember {
                    folder_id: fid(&m.folder_id)?,
                    conversation: m.conversation.into(),
                    state: m.state.into(),
                },
            });
        }
        for p in self.pins {
            c.pins.push(Stamped {
                hlc: p.hlc,
                value: core::FolderPins {
                    folder_id: fid(&p.folder_id)?,
                    pinned: p.pinned.into_iter().map(Into::into).collect(),
                },
            });
        }
        for r in self.removed {
            c.removed.push(Stamped {
                hlc: r.hlc,
                value: fid(&r.folder_id)?,
            });
        }
        if let Some(o) = self.order {
            let folder_ids = o
                .folder_ids
                .iter()
                .map(|f| fid(f))
                .collect::<Result<Vec<_>, _>>()?;
            c.order = Some(Stamped {
                hlc: o.hlc,
                value: core::FolderOrder { folder_ids },
            });
        }
        for m in self.saved {
            c.saved.push(m.into_core()?);
        }
        c.validate()
            .map_err(|e| FfiError::SyncedState(e.to_string()))?;
        Ok(c)
    }

    pub(crate) fn from_core(c: FolderChanges) -> Self {
        Self {
            metas: c.metas.into_iter().map(meta_register).collect(),
            members: c
                .members
                .into_iter()
                .map(|s| FolderMemberRegister {
                    hlc: s.hlc,
                    folder_id: s.value.folder_id.to_vec(),
                    conversation: s.value.conversation.into(),
                    state: match s.value.state {
                        MemberState::None => FolderMemberState::None,
                        MemberState::Included => FolderMemberState::Included,
                        MemberState::Excluded => FolderMemberState::Excluded,
                    },
                })
                .collect(),
            pins: c
                .pins
                .into_iter()
                .map(|s| FolderPinsRegister {
                    hlc: s.hlc,
                    folder_id: s.value.folder_id.to_vec(),
                    pinned: s.value.pinned.into_iter().map(Into::into).collect(),
                })
                .collect(),
            removed: c
                .removed
                .into_iter()
                .map(|s| FolderRemovedRegister {
                    hlc: s.hlc,
                    folder_id: s.value.to_vec(),
                })
                .collect(),
            order: c.order.map(|s| FolderOrderRegister {
                hlc: s.hlc,
                folder_ids: s.value.folder_ids.iter().map(|f| f.to_vec()).collect(),
            }),
            saved: c
                .saved
                .into_iter()
                .map(SavedMessageRegister::from_core)
                .collect(),
        }
    }
}

fn meta_register(s: Stamped<core::FolderMeta>) -> FolderMetaRegister {
    FolderMetaRegister {
        hlc: s.hlc,
        folder_id: s.value.folder_id.to_vec(),
        name: s.value.name,
        include_kinds: s.value.include_kinds,
        exclude_flags: s.value.exclude_flags,
    }
}

/// One part of the catch-up re-send (ADR-0058 section 5). Opaque: a client
/// gets parts from [`SyncedState::snapshot_parts`], sends each with
/// `DeviceGroup::create_devicegroup_message(identity,
/// DeviceGroupBody::StateSnapshot { part }, hlc)`, and on receipt hands the
/// parsed payload to [`SyncedState::apply_payload`].
#[derive(Debug, uniffi::Object)]
pub struct SyncedSnapshotPart {
    pub(crate) inner: core::StateSnapshot,
}

#[uniffi::export]
impl SyncedSnapshotPart {
    /// 0-based part index.
    pub fn part(&self) -> u32 {
        self.inner.part
    }

    /// Number of parts in this re-send. A receiver may log a missing part;
    /// parts fold independently.
    pub fn parts(&self) -> u32 {
        self.inner.parts
    }

    /// How many registers this part carries.
    pub fn entry_count(&self) -> u64 {
        self.inner.stamped_entries().len() as u64
    }
}

/// One folder as a client shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderView {
    /// 16 bytes.
    pub folder_id: Vec<u8>,
    /// Name and toggles. A folder is listed only once its name has arrived.
    pub meta: FolderMetaRegister,
    /// Conversations added by hand, sorted by `(kind, id)`.
    pub included: Vec<FolderConversationRef>,
    /// Conversations excluded by hand, sorted by `(kind, id)`.
    pub excluded: Vec<FolderConversationRef>,
    /// The folder's pins, in stored order.
    pub pinned: Vec<FolderConversationRef>,
}

/// A conversation's flags, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[allow(clippy::struct_excessive_bools)]
pub struct FlagsRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// The conversation's MLS group id.
    pub group_id: Vec<u8>,
    /// Pinned in the list.
    pub pinned: bool,
    /// Muted.
    pub muted: bool,
    /// Archived.
    pub archived: bool,
    /// Marked unread by hand.
    pub unread_override: bool,
}

/// A conversation's read cursor, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadCursorRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// The conversation's MLS group id.
    pub group_id: Vec<u8>,
    /// Read up to this message id.
    pub up_to_message_id: Vec<u8>,
    /// Wall-clock millis of the read. Display only.
    pub read_at_ms: u64,
}

/// A conversation's draft, stamped. Empty `text` = cleared at `hlc`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// The conversation's MLS group id.
    pub group_id: Vec<u8>,
    /// Composer text.
    pub text: String,
    /// Replying to this message id, or empty.
    pub reply_to_id: Vec<u8>,
}

/// The explicit conversation order, stamped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConversationOrderRegister {
    /// HLC stamp.
    pub hlc: u64,
    /// Group ids, top first.
    pub group_ids: Vec<Vec<u8>>,
}

fn flags_register(s: Stamped<core::FlagsEntry>) -> FlagsRegister {
    FlagsRegister {
        hlc: s.hlc,
        group_id: s.value.group_id,
        pinned: s.value.pinned,
        muted: s.value.muted,
        archived: s.value.archived,
        unread_override: s.value.unread_override,
    }
}

fn cursor_register(s: Stamped<core::ReadCursorEntry>) -> ReadCursorRegister {
    ReadCursorRegister {
        hlc: s.hlc,
        group_id: s.value.group_id,
        up_to_message_id: s.value.up_to_message_id,
        read_at_ms: s.value.read_at_ms,
    }
}

fn draft_register(s: Stamped<core::DraftEntry>) -> DraftRegister {
    DraftRegister {
        hlc: s.hlc,
        group_id: s.value.group_id,
        text: s.value.text,
        reply_to_id: s.value.reply_to_id,
    }
}

fn folder_view(v: core::FolderView) -> FolderView {
    FolderView {
        folder_id: v.folder_id.to_vec(),
        meta: meta_register(v.meta),
        included: v.included.into_iter().map(Into::into).collect(),
        excluded: v.excluded.into_iter().map(Into::into).collect(),
        pinned: v.pinned.into_iter().map(Into::into).collect(),
    }
}

/// The folded synced state of this device (ADR-0058 section 3).
///
/// Commutative, associative and idempotent: feed it every DeviceGroup
/// payload in whatever order they arrive, and every local write, and any two
/// devices that have seen the same writes hold the same state.
#[derive(Debug, uniffi::Object)]
pub struct SyncedState {
    inner: Mutex<core::SyncedState>,
}

impl SyncedState {
    fn with<T>(&self, f: impl FnOnce(&mut core::SyncedState) -> T) -> Result<T, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(f(&mut g))
    }
}

#[uniffi::export]
impl SyncedState {
    /// An empty state.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core::SyncedState::new()),
        })
    }

    /// Restore a state persisted with [`Self::to_bytes`], or carried in a
    /// backup payload. Fail closed: a malformed or out-of-bounds entry
    /// refuses the whole state.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`].
    #[uniffi::constructor]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let inner = core::SyncedState::from_bytes(&bytes)
            .map_err(|e| FfiError::SyncedState(e.to_string()))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(inner),
        }))
    }

    /// Persist the whole state (also what a backup payload carries).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, FfiError> {
        self.with(|s| s.to_bytes())
    }

    /// Fold every register `other` holds into this state, each with its
    /// original stamp (a restore merges the backup's state this way).
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn merge_from(&self, other: Arc<SyncedState>) -> Result<bool, FfiError> {
        // Take the other state's entries under its own lock, released before
        // this one is taken, so merging a state into itself cannot deadlock.
        let entries = other.with(|o| o.entries())?;
        self.with(|s| s.apply_all(entries))
    }

    /// Fold one DeviceGroup payload in: any of the six kinds, received from
    /// another device or built locally for this device's own write (a
    /// device never receives its own messages back, so it folds each write
    /// it sends). Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] when a locally built `FolderChanges` breaks
    /// a bound (a parsed one never does: the decoder refused it already).
    pub fn apply_payload(&self, payload: ParsedDeviceGroupPayload) -> Result<bool, FfiError> {
        let hlc = payload.hlc;
        let body = payload.body.into_mls()?;
        let entries = body.stamped_entries(hlc);
        self.with(|s| s.apply_all(entries))
    }

    /// Fold a local folder edit, or a local Save / Remove of a saved
    /// message, in (the same [`FolderChangeSet`] the client then sends).
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] when the change set breaks a bound.
    pub fn apply_folder_changes(&self, changes: FolderChangeSet) -> Result<bool, FfiError> {
        let c = changes.into_core()?;
        self.with(|s| s.apply_all(c.stamped_entries()))
    }

    /// Every live folder, in the resolved order (ADR-0058 section 3).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn folders(&self) -> Result<Vec<FolderView>, FfiError> {
        self.with(|s| s.folders().into_iter().map(folder_view).collect())
    }

    /// Flags held for a conversation.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn flags(&self, group_id: Vec<u8>) -> Result<Option<FlagsRegister>, FfiError> {
        self.with(|s| s.flags(&group_id).map(flags_register))
    }

    /// Read cursor held for a conversation.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn read_cursor(&self, group_id: Vec<u8>) -> Result<Option<ReadCursorRegister>, FfiError> {
        self.with(|s| s.read_cursor(&group_id).map(cursor_register))
    }

    /// Draft held for a conversation, a cleared one (empty text) included.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn draft(&self, group_id: Vec<u8>) -> Result<Option<DraftRegister>, FfiError> {
        self.with(|s| s.draft(&group_id).map(draft_register))
    }

    /// Every flags register.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_flags(&self) -> Result<Vec<FlagsRegister>, FfiError> {
        self.with(|s| s.all_flags().into_iter().map(flags_register).collect())
    }

    /// Every read-cursor register.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_read_cursors(&self) -> Result<Vec<ReadCursorRegister>, FfiError> {
        self.with(|s| {
            s.all_read_cursors()
                .into_iter()
                .map(cursor_register)
                .collect()
        })
    }

    /// Every draft register, cleared ones included.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_drafts(&self) -> Result<Vec<DraftRegister>, FfiError> {
        self.with(|s| s.all_drafts().into_iter().map(draft_register).collect())
    }

    /// The explicit conversation order, when held.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn conversation_order(&self) -> Result<Option<ConversationOrderRegister>, FfiError> {
        self.with(|s| {
            s.conversation_order().map(|o| ConversationOrderRegister {
                hlc: o.hlc,
                group_ids: o.value.group_ids,
            })
        })
    }

    /// The account's saved messages, most recently saved first (ADR-0065
    /// section 4). Removed ones are not listed.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn saved(&self) -> Result<Vec<SavedMessageView>, FfiError> {
        self.with(|s| {
            s.saved()
                .into_iter()
                .map(|v| SavedMessageView {
                    hlc: v.hlc,
                    conversation: v.conversation.into(),
                    message_id: v.message_id,
                    saved_at_ms: v.saved_at_ms,
                    author: v.snapshot.author,
                    excerpt: v.snapshot.excerpt,
                    attachment_kind: v.snapshot.attachment_kind,
                    blob_ids: v.blob_ids.iter().map(|b| b.to_vec()).collect(),
                })
                .collect()
        })
    }

    /// Whether this message is saved now (the row label and the saved
    /// mark). `false` for a message never saved, for a removed one, and for
    /// a conversation reference that is not well formed.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn is_saved(
        &self,
        conversation: FolderConversationRef,
        message_id: u64,
    ) -> Result<bool, FfiError> {
        let conversation: ConversationRef = conversation.into();
        self.with(|s| s.is_saved(&conversation, message_id))
    }

    /// Every blob id (32 bytes) a saved message names, each once, in
    /// bytewise order: what this device sends keeps for (ADR-0065
    /// section 3).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn saved_blob_ids(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.saved_blob_ids().iter().map(|b| b.to_vec()).collect())
    }

    /// The greatest stamp held, or 0. Seed the HLC above it after a restore.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn max_hlc(&self) -> Result<u64, FfiError> {
        self.with(|s| s.max_hlc())
    }

    /// The catch-up re-send (ADR-0058 section 5.1): the whole state, every
    /// entry with its original stamp, split so each encoded part is at most
    /// 256 KiB. Always at least one part.
    ///
    /// # Errors
    ///
    /// [`FfiError::DeviceGroupPayloadRefused`] when the state needs more than
    /// 64 parts.
    pub fn snapshot_parts(&self) -> Result<Vec<Arc<SyncedSnapshotPart>>, FfiError> {
        let snap = self.with(|s| s.snapshot())?;
        let parts = lakko_mls::device_group_sync::split_state_snapshot(
            snap,
            core::SNAPSHOT_MAX_PART_BYTES,
        )?;
        Ok(parts
            .into_iter()
            .map(|inner| Arc::new(SyncedSnapshotPart { inner }))
            .collect())
    }
}

/// Whether this device publishes the catch-up snapshot after the
/// DeviceGroup grew (ADR-0058 section 5.1): only the lowest leaf among the
/// members BEFORE the growth. Wait a random delay of up to 30 seconds first.
#[uniffi::export]
pub fn should_send_snapshot(own_leaf: u32, prior_member_leaves: Vec<u32>) -> bool {
    core::should_send_snapshot(own_leaf, &prior_member_leaves)
}

/// Whether `name` is an acceptable folder name as it stands: 1..=24
/// characters, at most 96 bytes, the display-name character rule, no
/// leading or trailing whitespace. The editor trims before asking.
#[uniffi::export]
pub fn is_valid_folder_name(name: String) -> bool {
    core::check_folder_name(&name).is_ok()
}

/// A fresh random 16-byte folder id, minted once when a folder is created.
#[uniffi::export]
pub fn mint_folder_id() -> Vec<u8> {
    let mut id = [0u8; core::FOLDER_ID_LEN];
    lakko_crypto::random::fill(&mut id);
    id.to_vec()
}

/// Build the register that saves a message (ADR-0065 section 4), stamped
/// `hlc`.
///
/// Pass what the client holds: `author` and `excerpt` are cut to their
/// bounds (255 and 512 bytes) on a character boundary, so the whole message
/// text may be passed as `excerpt`; a blob id named twice is kept once.
/// `attachment_kind` is a short token (empty when the message has no
/// attachment) and `blob_ids` the 32-byte ids of the message's files and
/// thumbnails.
///
/// # Errors
///
/// [`FfiError::SyncedState`] when a blob id is not 32 bytes, more than 128
/// distinct blob ids are named, `attachment_kind` is longer than 32 bytes,
/// or the conversation id is empty or longer than 64 bytes.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn compose_saved_message(
    hlc: u64,
    conversation: FolderConversationRef,
    message_id: u64,
    saved_at_ms: u64,
    author: String,
    excerpt: String,
    attachment_kind: String,
    blob_ids: Vec<Vec<u8>>,
) -> Result<SavedMessageRegister, FfiError> {
    let saved = core::SavedMessage::present(
        conversation.into(),
        message_id,
        saved_at_ms,
        &core::SavedSnapshot {
            author,
            excerpt,
            attachment_kind,
        },
        &blob_ids,
    )
    .map_err(synced_err)?;
    Ok(SavedMessageRegister::from_core(Stamped {
        hlc,
        value: saved,
    }))
}

/// Build the register that removes a message from saved, stamped `hlc`. It
/// is kept as a tombstone on every device; saving the message again later
/// (a greater stamp) brings it back.
///
/// # Errors
///
/// [`FfiError::SyncedState`] when the conversation id is empty or longer
/// than 64 bytes.
#[uniffi::export]
pub fn compose_saved_removal(
    hlc: u64,
    conversation: FolderConversationRef,
    message_id: u64,
) -> Result<SavedMessageRegister, FfiError> {
    let saved = core::SavedMessage::removed(conversation.into(), message_id).map_err(synced_err)?;
    Ok(SavedMessageRegister::from_core(Stamped {
        hlc,
        value: saved,
    }))
}

/// How many saved messages a client lets one account hold: refuse a new
/// Save once [`SyncedState::saved`] lists this many. A client-side limit,
/// chosen; NOT a decode bound (two devices that each saved while offline
/// can exceed it together, and that state still decodes and folds).
#[uniffi::export]
#[must_use]
pub fn saved_max_client() -> u32 {
    u32::try_from(core::SAVED_MAX_CLIENT).unwrap_or(u32::MAX)
}

/// How many saved registers a client puts in one change set. A larger
/// selection (Select mode's Save) goes out as several change sets, so one
/// DeviceGroup message stays well under what the Delivery Service accepts.
/// A client-side sending rule, chosen; NOT a decode bound.
#[uniffi::export]
#[must_use]
pub fn saved_max_per_change_set() -> u32 {
    u32::try_from(core::SAVED_MAX_PER_CHANGE_SET).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceGroupBody;

    fn change_set() -> FolderChangeSet {
        FolderChangeSet {
            metas: vec![FolderMetaRegister {
                hlc: 5,
                folder_id: vec![1; 16],
                name: "Work".into(),
                include_kinds: 0b010,
                exclude_flags: 0b100,
            }],
            members: vec![FolderMemberRegister {
                hlc: 6,
                folder_id: vec![1; 16],
                conversation: FolderConversationRef {
                    kind: FolderConversationKind::Channel,
                    id: vec![7; 32],
                },
                state: FolderMemberState::Included,
            }],
            ..FolderChangeSet::default()
        }
    }

    #[test]
    fn a_local_edit_folds_and_reads_back() {
        let s = SyncedState::new();
        assert!(s.apply_folder_changes(change_set()).unwrap());
        assert!(!s.apply_folder_changes(change_set()).unwrap());
        let f = s.folders().unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].meta.name, "Work");
        assert_eq!(f[0].included[0].kind, FolderConversationKind::Channel);
        let restored = SyncedState::from_bytes(s.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.folders().unwrap(), f);
    }

    #[test]
    fn an_out_of_bounds_local_edit_is_refused() {
        let mut c = change_set();
        c.metas[0].folder_id = vec![1; 15];
        assert!(matches!(
            SyncedState::new().apply_folder_changes(c),
            Err(FfiError::SyncedState(_))
        ));
        let mut c = change_set();
        c.metas[0].name = String::new();
        assert!(matches!(
            SyncedState::new().apply_folder_changes(c),
            Err(FfiError::SyncedState(_))
        ));
    }

    #[test]
    fn a_snapshot_part_rebuilds_the_state_through_a_payload() {
        let a = SyncedState::new();
        a.apply_folder_changes(change_set()).unwrap();
        let b = SyncedState::new();
        for part in a.snapshot_parts().unwrap() {
            let payload = ParsedDeviceGroupPayload {
                hlc: 1,
                sender_leaf: 0,
                body: DeviceGroupBody::StateSnapshot { part },
            };
            b.apply_payload(payload).unwrap();
        }
        assert_eq!(b.to_bytes().unwrap(), a.to_bytes().unwrap());
    }

    fn chat() -> FolderConversationRef {
        FolderConversationRef {
            kind: FolderConversationKind::Group,
            id: vec![9; 32],
        }
    }

    fn save(hlc: u64, message_id: u64, saved_at_ms: u64, blobs: &[u8]) -> FolderChangeSet {
        FolderChangeSet {
            saved: vec![compose_saved_message(
                hlc,
                chat(),
                message_id,
                saved_at_ms,
                "Ada".into(),
                format!("message {message_id}"),
                if blobs.is_empty() { "" } else { "photo" }.into(),
                blobs.iter().map(|b| vec![*b; 32]).collect(),
            )
            .unwrap()],
            ..FolderChangeSet::default()
        }
    }

    fn remove(hlc: u64, message_id: u64) -> FolderChangeSet {
        FolderChangeSet {
            saved: vec![compose_saved_removal(hlc, chat(), message_id).unwrap()],
            ..FolderChangeSet::default()
        }
    }

    #[test]
    fn save_list_remove_and_save_again() {
        let s = SyncedState::new();
        assert!(!s.is_saved(chat(), 7).unwrap());
        assert!(s.apply_folder_changes(save(10, 7, 100, &[1, 2])).unwrap());
        // The same write again changes nothing.
        assert!(!s.apply_folder_changes(save(10, 7, 100, &[1, 2])).unwrap());
        assert!(s.apply_folder_changes(save(11, 8, 200, &[2, 3])).unwrap());
        assert!(s.is_saved(chat(), 7).unwrap());

        // Most recently saved first.
        let list = s.saved().unwrap();
        assert_eq!(
            list.iter().map(|v| v.message_id).collect::<Vec<_>>(),
            vec![8, 7]
        );
        assert_eq!(list[1].hlc, 10);
        assert_eq!(list[1].saved_at_ms, 100);
        assert_eq!(list[1].author, "Ada");
        assert_eq!(list[1].excerpt, "message 7");
        assert_eq!(list[1].attachment_kind, "photo");
        assert_eq!(list[1].conversation, chat());
        assert_eq!(list[1].blob_ids, vec![vec![1u8; 32], vec![2; 32]]);
        // Each blob id once, across messages.
        assert_eq!(
            s.saved_blob_ids().unwrap(),
            vec![vec![1u8; 32], vec![2; 32], vec![3; 32]]
        );

        // Remove: gone from every read, kept as a tombstone, and an older
        // save cannot bring it back.
        assert!(s.apply_folder_changes(remove(12, 7)).unwrap());
        assert!(!s.is_saved(chat(), 7).unwrap());
        assert_eq!(s.saved().unwrap().len(), 1);
        assert_eq!(
            s.saved_blob_ids().unwrap(),
            vec![vec![2u8; 32], vec![3; 32]]
        );
        assert!(!s.apply_folder_changes(save(10, 7, 100, &[1, 2])).unwrap());
        assert!(!s.is_saved(chat(), 7).unwrap());

        // Saved again, with a greater stamp.
        assert!(s.apply_folder_changes(save(13, 7, 300, &[])).unwrap());
        assert!(s.is_saved(chat(), 7).unwrap());
        assert_eq!(s.saved().unwrap()[0].message_id, 7);
        assert_eq!(s.saved().unwrap()[0].attachment_kind, "");
        assert_eq!(s.max_hlc().unwrap(), 13);
    }

    #[test]
    fn saved_messages_persist_and_reach_a_late_device_in_the_snapshot() {
        let a = SyncedState::new();
        a.apply_folder_changes(change_set()).unwrap();
        a.apply_folder_changes(save(10, 7, 100, &[1])).unwrap();
        a.apply_folder_changes(save(11, 8, 200, &[])).unwrap();
        a.apply_folder_changes(remove(12, 8)).unwrap();

        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.saved().unwrap(), a.saved().unwrap());
        assert_eq!(restored.to_bytes().unwrap(), a.to_bytes().unwrap());

        let late = SyncedState::new();
        for part in a.snapshot_parts().unwrap() {
            late.apply_payload(ParsedDeviceGroupPayload {
                hlc: 1,
                sender_leaf: 0,
                body: DeviceGroupBody::StateSnapshot { part },
            })
            .unwrap();
        }
        assert_eq!(late.to_bytes().unwrap(), a.to_bytes().unwrap());
        assert!(late.is_saved(chat(), 7).unwrap());
        // The tombstone travelled too.
        assert!(!late.is_saved(chat(), 8).unwrap());
        assert!(!late.apply_folder_changes(save(11, 8, 200, &[])).unwrap());

        // A backup restore merges.
        let fresh = SyncedState::new();
        fresh.apply_folder_changes(save(20, 9, 400, &[5])).unwrap();
        assert!(fresh.merge_from(restored).unwrap());
        assert_eq!(
            fresh
                .saved()
                .unwrap()
                .iter()
                .map(|v| v.message_id)
                .collect::<Vec<_>>(),
            vec![9, 7]
        );
    }

    #[test]
    fn a_received_save_crosses_the_wire_and_folds() {
        use lakko_proto::mls_app::devicegroup::v1 as pb;
        use prost::Message as _;
        // What another device sent: one save and one removal, each with its
        // own stamp; the envelope stamp is not theirs.
        let wire = pb::DeviceGroupPayload {
            hlc: 1,
            sender_leaf: 2,
            body: Some(pb::device_group_payload::Body::FolderChanges(
                pb::FolderChanges {
                    saved: vec![
                        pb::SavedMessage {
                            hlc: 50,
                            conversation: Some(pb::ConversationRef {
                                kind: 2,
                                id: vec![9; 32],
                            }),
                            message_id: 7,
                            state: 1,
                            saved_at_ms: 100,
                            author: "Ada".into(),
                            excerpt: "hello".into(),
                            attachment_kind: "voice".into(),
                            blob_ids: vec![vec![4; 32]],
                        },
                        pb::SavedMessage {
                            hlc: 51,
                            conversation: Some(pb::ConversationRef {
                                kind: 2,
                                id: vec![9; 32],
                            }),
                            message_id: 8,
                            state: 2,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            )),
        }
        .encode_to_vec();
        let parsed = crate::parse_devicegroup_payload(wire).unwrap();
        let DeviceGroupBody::FolderChanges { changes } = &parsed.body else {
            panic!("not a change set");
        };
        // The parsed body is the same record a client composes.
        assert_eq!(
            changes.saved[0],
            compose_saved_message(
                50,
                chat(),
                7,
                100,
                "Ada".into(),
                "hello".into(),
                "voice".into(),
                vec![vec![4; 32]],
            )
            .unwrap()
        );
        assert_eq!(
            changes.saved[1],
            compose_saved_removal(51, chat(), 8).unwrap()
        );

        let s = SyncedState::new();
        s.apply_folder_changes(save(40, 8, 10, &[])).unwrap();
        assert!(s.apply_payload(parsed).unwrap());
        assert!(s.is_saved(chat(), 7).unwrap());
        assert!(!s.is_saved(chat(), 8).unwrap());
        assert_eq!(s.saved().unwrap()[0].hlc, 50);

        // An out-of-bounds register refuses the whole payload at the parser.
        let bad = pb::DeviceGroupPayload {
            hlc: 1,
            sender_leaf: 2,
            body: Some(pb::device_group_payload::Body::FolderChanges(
                pb::FolderChanges {
                    saved: vec![pb::SavedMessage {
                        hlc: 50,
                        conversation: Some(pb::ConversationRef {
                            kind: 2,
                            id: vec![9; 32],
                        }),
                        message_id: 7,
                        state: 1,
                        blob_ids: vec![vec![4; 31]],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )),
        }
        .encode_to_vec();
        assert!(matches!(
            crate::parse_devicegroup_payload(bad),
            Err(FfiError::DeviceGroupPayloadRefused(_))
        ));
    }

    #[test]
    fn composing_a_save_fits_the_text_and_refuses_what_it_cannot_repair() {
        let long = "😀".repeat(300);
        let reg = compose_saved_message(
            1,
            chat(),
            7,
            5,
            long.clone(),
            long.clone(),
            "file".into(),
            vec![vec![1; 32], vec![1; 32], vec![2; 32]],
        )
        .unwrap();
        let SavedMessageValue::Present {
            author,
            excerpt,
            blob_ids,
            ..
        } = &reg.value
        else {
            panic!("not a save");
        };
        assert_eq!(excerpt.len(), 512);
        assert_eq!(author.len(), 252);
        assert!(long.starts_with(excerpt.as_str()));
        assert_eq!(blob_ids, &vec![vec![1u8; 32], vec![2; 32]]);
        // The composed register is accepted as it stands.
        let s = SyncedState::new();
        assert!(s
            .apply_folder_changes(FolderChangeSet {
                saved: vec![reg],
                ..FolderChangeSet::default()
            })
            .unwrap());

        let refused = |r: Result<SavedMessageRegister, FfiError>| {
            assert!(matches!(r, Err(FfiError::SyncedState(_))));
        };
        refused(compose_saved_message(
            1,
            chat(),
            7,
            5,
            String::new(),
            String::new(),
            String::new(),
            vec![vec![1; 31]],
        ));
        refused(compose_saved_message(
            1,
            chat(),
            7,
            5,
            String::new(),
            String::new(),
            "k".repeat(33),
            vec![],
        ));
        refused(compose_saved_message(
            1,
            chat(),
            7,
            5,
            String::new(),
            String::new(),
            String::new(),
            (0..129u8).map(|i| vec![i; 32]).collect(),
        ));
        let no_chat = FolderConversationRef {
            kind: FolderConversationKind::OneToOne,
            id: vec![],
        };
        refused(compose_saved_message(
            1,
            no_chat.clone(),
            7,
            5,
            String::new(),
            String::new(),
            String::new(),
            vec![],
        ));
        refused(compose_saved_removal(1, no_chat, 7));
    }

    #[test]
    fn a_register_built_by_hand_is_held_to_the_bounds_not_repaired() {
        let by_hand = |value: SavedMessageValue| {
            SyncedState::new().apply_folder_changes(FolderChangeSet {
                saved: vec![SavedMessageRegister {
                    hlc: 1,
                    conversation: chat(),
                    message_id: 1,
                    value,
                }],
                ..FolderChangeSet::default()
            })
        };
        let present = |excerpt: String, blob_ids: Vec<Vec<u8>>| SavedMessageValue::Present {
            saved_at_ms: 1,
            author: String::new(),
            excerpt,
            attachment_kind: String::new(),
            blob_ids,
        };
        assert!(matches!(
            by_hand(present("e".repeat(513), vec![])),
            Err(FfiError::SyncedState(_))
        ));
        assert!(matches!(
            by_hand(present(String::new(), vec![vec![1; 32], vec![1; 32]])),
            Err(FfiError::SyncedState(_))
        ));
        assert!(matches!(
            by_hand(present(String::new(), vec![vec![1; 33]])),
            Err(FfiError::SyncedState(_))
        ));
        assert!(by_hand(present("e".repeat(512), vec![vec![1; 32]])).unwrap());
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

    #[test]
    fn helpers() {
        assert!(should_send_snapshot(1, vec![4, 1]));
        assert!(!should_send_snapshot(4, vec![4, 1]));
        assert!(is_valid_folder_name("Work".into()));
        assert!(!is_valid_folder_name(" Work".into()));
        assert_eq!(mint_folder_id().len(), 16);
    }
}
