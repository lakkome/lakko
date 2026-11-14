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
//!
//! ## Conversation ends (ADR-0087 section 9)
//!
//! One more register, carried in [`FolderChangeSet::conversation_ends`]. To
//! write one:
//!
//! 1. ask [`SyncedState::compose_conversation_ended`] or
//!    [`SyncedState::compose_conversation_joined`], passing the MLS epoch
//!    (`Group.epoch()`; `0` for a channel). `None` means there is nothing to
//!    tell the other devices: go on with the rest of the end without it;
//! 2. put the register in a [`FolderChangeSet`], fold it with
//!    [`SyncedState::apply_folder_changes`] and send the same change set as
//!    `DeviceGroupBody::FolderChanges`.
//!
//! A received one arrives through [`SyncedState::apply_payload`]; read
//! [`SyncedState::ended_conversations`] after every apply and at launch.
//! The register's `epoch` is what `Group::delete_after_ended_elsewhere` takes.
//! A device holding an active group at an epoch ABOVE a register's has come
//! back since that end: it does not act on the register, and answers with
//! `compose_conversation_joined` at its own epoch.

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
    /// Conversation-end registers (ADR-0087 section 9). Defaults to empty.
    #[uniffi(default = [])]
    pub conversation_ends: Vec<ConversationEndRegister>,
}

/// What a conversation-end register says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConversationEndState {
    /// No device of this account is left in the conversation.
    Ended,
    /// The account joined again after an end.
    Joined,
}

/// How this account's membership of a conversation stands, stamped (ADR-0087
/// section 9). Build it with [`SyncedState::compose_conversation_ended`] /
/// [`SyncedState::compose_conversation_joined`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConversationEndRegister {
    /// HLC stamp of this register.
    pub hlc: u64,
    /// The conversation.
    pub conversation: FolderConversationRef,
    /// Ended, or joined again.
    pub state: ConversationEndState,
    /// The MLS epoch of the end or the join; `0` for a channel. The fold
    /// orders by this before the stamp.
    pub epoch: u64,
}

impl ConversationEndRegister {
    fn into_core(self) -> Stamped<core::ConversationEnd> {
        Stamped {
            hlc: self.hlc,
            value: core::ConversationEnd {
                conversation: self.conversation.into(),
                state: match self.state {
                    ConversationEndState::Ended => core::ConversationEndState::Ended,
                    ConversationEndState::Joined => core::ConversationEndState::Joined,
                },
                epoch: self.epoch,
            },
        }
    }

    fn from_core(s: Stamped<core::ConversationEnd>) -> Self {
        Self {
            hlc: s.hlc,
            conversation: s.value.conversation.into(),
            state: match s.value.state {
                core::ConversationEndState::Ended => ConversationEndState::Ended,
                core::ConversationEndState::Joined => ConversationEndState::Joined,
            },
            epoch: s.value.epoch,
        }
    }
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
        /// The message's own time in UNIX millis as the saving device showed
        /// it; `0` = unknown. Display only.
        #[uniffi(default = 0)]
        sent_at_ms: u64,
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
    /// The message's own time in UNIX millis as the saving device showed
    /// it, so the row can show the message date before this device holds
    /// the message; `0` = unknown.
    #[uniffi(default = 0)]
    pub sent_at_ms: u64,
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
                sent_at_ms,
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
                        sent_at_ms,
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
                    sent_at_ms: snapshot.sent_at_ms,
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
        c.conversation_ends = self
            .conversation_ends
            .into_iter()
            .map(ConversationEndRegister::into_core)
            .collect();
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
            conversation_ends: c
                .conversation_ends
                .into_iter()
                .map(ConversationEndRegister::from_core)
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

/// One conversation-lineage register (ADR-0073 R.5): the group
/// `successor_group_id` continues the conversation `root_conversation_id`
/// (the group id of the first group of the lineage).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LineageRegister {
    /// HLC stamp the register was written at.
    pub hlc: u64,
    /// MLS group id of the group that continues the conversation.
    pub successor_group_id: Vec<u8>,
    /// The conversation id every person-facing store is keyed on.
    pub root_conversation_id: Vec<u8>,
}

fn lineage_register(s: Stamped<core::LineageEntry>) -> LineageRegister {
    LineageRegister {
        hlc: s.hlc,
        successor_group_id: s.value.successor_group_id,
        root_conversation_id: s.value.root_conversation_id,
    }
}

fn lineage_refusal(e: core::LineageEnvelopeError) -> FfiError {
    FfiError::SyncedState(format!("{}: {e}", e.code()))
}

/// The account's recovery email register as held (ADR-0075, amended
/// 2026-10-13), a removal included.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecoveryEmailRegister {
    /// HLC stamp the register was written at.
    pub hlc: u64,
    /// The normalised address, or `None` when the register records that the
    /// account has no recovery email (a removal).
    pub address: Option<String>,
}

fn recovery_email_register(s: Stamped<core::RecoveryEmailEntry>) -> RecoveryEmailRegister {
    RecoveryEmailRegister {
        hlc: s.hlc,
        address: Some(s.value.address).filter(|a| !a.is_empty()),
    }
}

/// The account's own-username register as held (ADR-0076 section 3), a
/// stamped "none" included.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OwnUsernameRegister {
    /// HLC stamp the register was written at.
    pub hlc: u64,
    /// The canonical username, or `None` when the register records that the
    /// account has no username.
    pub username: Option<String>,
}

fn own_username_register(s: Stamped<core::OwnUsernameEntry>) -> OwnUsernameRegister {
    OwnUsernameRegister {
        hlc: s.hlc,
        username: Some(s.value.username).filter(|u| !u.is_empty()),
    }
}

/// Which ledger an invite link lives in (ADR-0079).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LinkRecordKind {
    /// A contact invite link (`/i/`).
    Contact,
    /// A call invite link (`/c/`).
    Call,
    /// A group invite link (`/g/`; ADR-0087 phase B, appended 2026-11-05).
    Group,
    /// A channel subscriber invitation (`/g/` preview kind 3).
    Channel,
    /// A channel publisher invitation (`/g/` preview kind 2).
    Publisher,
}

impl From<LinkRecordKind> for core::LinkRecordKind {
    fn from(k: LinkRecordKind) -> Self {
        match k {
            LinkRecordKind::Contact => Self::Contact,
            LinkRecordKind::Call => Self::Call,
            LinkRecordKind::Group => Self::Group,
            LinkRecordKind::Channel => Self::Channel,
            LinkRecordKind::Publisher => Self::Publisher,
        }
    }
}

impl From<core::LinkRecordKind> for LinkRecordKind {
    fn from(k: core::LinkRecordKind) -> Self {
        match k {
            core::LinkRecordKind::Contact => Self::Contact,
            core::LinkRecordKind::Call => Self::Call,
            core::LinkRecordKind::Group => Self::Group,
            core::LinkRecordKind::Channel => Self::Channel,
            core::LinkRecordKind::Publisher => Self::Publisher,
        }
    }
}

/// What to do with a contact request that carries a link answer
/// (ADR-0079 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LinkAnswerVerdict {
    /// A live contact link of this account, secret matching: accept the
    /// request without showing a card, then [`SyncedState::record_link_use`].
    Accept,
    /// The requester was already accepted through this link: show nothing.
    Answered,
    /// The link does not count for the request (wrong secret, revoked now,
    /// expired before the request was written, used up): drop the request
    /// silently and answer the requester with a link refusal (ADR-0079,
    /// 2026-10-19).
    Dead,
    /// This device holds no record of the link: keep the request undecided
    /// and unshown, ask again when the synced state changes. Never a refusal.
    Unknown,
}

impl From<core::LinkAnswerVerdict> for LinkAnswerVerdict {
    fn from(v: core::LinkAnswerVerdict) -> Self {
        match v {
            core::LinkAnswerVerdict::Accept => Self::Accept,
            core::LinkAnswerVerdict::Answered => Self::Answered,
            core::LinkAnswerVerdict::Dead => Self::Dead,
            core::LinkAnswerVerdict::Unknown => Self::Unknown,
        }
    }
}

/// One of the account's invite links (ADR-0079 section 4). `link_secret`
/// and `revocation_seed` are SECRET: store them only inside the synced
/// state, never log them.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LinkRecord {
    /// HLC stamp the register was written at (ignored on write).
    pub hlc: u64,
    /// The link's 16-byte id.
    pub link_id: Vec<u8>,
    /// The link's 12-byte secret (the URL fragment).
    pub link_secret: Vec<u8>,
    /// Which ledger.
    pub kind: LinkRecordKind,
    /// The 6-byte ledger id (every link has a ledger row; the inline form was
    /// removed on 2026-10-19).
    pub ledger_id: Vec<u8>,
    /// The 32-byte seed that deletes the ledger row (ADR-0078 section 10).
    /// Any device of the account
    /// that revokes the link deletes the row with it.
    pub revocation_seed: Vec<u8>,
    /// UNIX millis after which the link no longer works; 0 = never.
    pub expiry_ms: u64,
    /// The use limit as the capability carries it (0 = single use,
    /// `0xFFFFFFFF` = no limit).
    pub max_uses: u32,
    /// Requests accepted through the link so far.
    pub uses: u32,
    /// Revoked (final).
    pub revoked: bool,
    /// UNIX millis at which the link was minted.
    pub minted_at_ms: u64,
    /// The owner's label, or empty.
    pub label: String,
    /// Account keys (32 bytes each) of the requesters accepted through it.
    pub answered: Vec<Vec<u8>>,
    /// Copied on one of the account's devices (final, 2026-10-26). Set it
    /// with [`SyncedState::mark_link_shared`], not on write.
    #[uniffi(default = false)]
    pub shared: bool,
    /// The link's terms may still be changed: not revoked, never copied,
    /// never answered. Computed by the core on read; ignored on write.
    #[uniffi(default = false)]
    pub editable: bool,
    /// What a `/g/` link targets (ADR-0087 phase B): the 32-byte group id of
    /// a group link, the 16-byte channel id of a channel or publisher link;
    /// empty for `/i/` and `/c/`. Byte fields carry no default (Swift `Data`
    /// has no literal one): pass empty for a contact or call link.
    pub target_id: Vec<u8>,
    /// A private channel subscriber link's 32-byte invite seed, else empty.
    /// SECRET.
    pub invite_seed: Vec<u8>,
    /// The admission-key version a private channel subscriber link names; 0
    /// for every other link.
    #[uniffi(default = 0)]
    pub admission_version: u16,
    /// The sealed `/g/` blob the ledger holds (what an invitation card
    /// carries), at most `lakko_contacts::group_invite::MAX_SEALED_LEN`
    /// bytes; empty for `/i/` and `/c/`.
    pub capability: Vec<u8>,
}

fn link_record(s: Stamped<core::LinkRecordEntry>) -> LinkRecord {
    let editable = s.value.editable();
    LinkRecord {
        hlc: s.hlc,
        link_id: s.value.link_id.to_vec(),
        link_secret: s.value.link_secret.to_vec(),
        kind: s.value.kind.into(),
        ledger_id: s.value.ledger_id,
        revocation_seed: s.value.revocation_seed,
        expiry_ms: s.value.expiry_ms,
        max_uses: s.value.max_uses,
        uses: s.value.uses,
        revoked: s.value.revoked,
        minted_at_ms: s.value.minted_at_ms,
        editable,
        shared: s.value.shared,
        label: s.value.label,
        answered: s.value.answered.iter().map(|k| k.to_vec()).collect(),
        target_id: s.value.target_id,
        invite_seed: s.value.invite_seed,
        admission_version: s.value.admission_version,
        capability: s.value.capability,
    }
}

fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes)
        .map_err(|_| FfiError::SyncedState(format!("bounds: {what} must be {N} bytes")))
}

fn link_record_entry(r: LinkRecord) -> Result<core::LinkRecordEntry, FfiError> {
    Ok(core::LinkRecordEntry {
        link_id: fixed(&r.link_id, "link id")?,
        link_secret: fixed(&r.link_secret, "link secret")?,
        kind: r.kind.into(),
        ledger_id: r.ledger_id,
        revocation_seed: r.revocation_seed,
        expiry_ms: r.expiry_ms,
        max_uses: r.max_uses,
        uses: r.uses,
        revoked: r.revoked,
        shared: r.shared,
        minted_at_ms: r.minted_at_ms,
        label: r.label,
        answered: r
            .answered
            .iter()
            .map(|k| fixed(k, "requester key"))
            .collect::<Result<Vec<_>, _>>()?,
        target_id: r.target_id,
        invite_seed: r.invite_seed,
        admission_version: r.admission_version,
        capability: r.capability,
    })
}

/// The register id of a `/g/` link (a group, channel subscriber or publisher
/// invitation) in the synced link register: a BLAKE3 derive-key of its
/// 6-byte ledger id, truncated to 16 bytes (ADR-0087 phase B). A `LinkRecord`
/// of kind `Group` / `Channel` / `Publisher` must carry exactly this
/// `link_id`.
///
/// # Errors
///
/// [`FfiError::SyncedState`] when `ledger_id` is not 6 bytes.
#[uniffi::export]
pub fn group_link_record_id(ledger_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let ledger: [u8; core::LINK_RECORD_LEDGER_ID_LEN] = fixed(&ledger_id, "ledger id")?;
    Ok(lakko_contacts::group_invite::group_link_record_id(&ledger).to_vec())
}

/// What the account is in a channel (2026-11-13). A second device of an
/// `Owner` or a `CoPublisher` is a reader for now: it lists the channel and
/// reads its posts, and shows no owner or publisher control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelSubscriptionRole {
    /// A reader.
    Subscriber,
    /// The channel's owner.
    Owner,
    /// A co-publisher ("publisher" on screen).
    CoPublisher,
}

impl From<ChannelSubscriptionRole> for core::ChannelSubscriptionRole {
    fn from(r: ChannelSubscriptionRole) -> Self {
        match r {
            ChannelSubscriptionRole::Subscriber => Self::Subscriber,
            ChannelSubscriptionRole::Owner => Self::Owner,
            ChannelSubscriptionRole::CoPublisher => Self::CoPublisher,
        }
    }
}

impl From<core::ChannelSubscriptionRole> for ChannelSubscriptionRole {
    fn from(r: core::ChannelSubscriptionRole) -> Self {
        match r {
            core::ChannelSubscriptionRole::Subscriber => Self::Subscriber,
            core::ChannelSubscriptionRole::Owner => Self::Owner,
            core::ChannelSubscriptionRole::CoPublisher => Self::CoPublisher,
        }
    }
}

/// One channel-subscription register (2026-11-13): a live subscription, or
/// the leave of one (`subscribed == false`, every other field empty and
/// `role` `Subscriber`). `subscriber_secret` and `read_key` are SECRET:
/// keep them where the client keeps its subscriber keys, never log them.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct ChannelSubscriptionRegister {
    /// HLC stamp the register was written at.
    pub hlc: u64,
    /// The 16-byte channel id.
    pub channel_id: Vec<u8>,
    /// `true` while the account is subscribed; `false` after a leave.
    pub subscribed: bool,
    /// What the account is in the channel.
    pub role: ChannelSubscriptionRole,
    /// The 32-byte Ed25519 seed of the account's ONE subscriber key for the
    /// channel (`ChannelSubscriberKeypair.fromSecret`), shared by its
    /// devices. SECRET.
    pub subscriber_secret: Vec<u8>,
    /// The channel's 32-byte read key, or empty. SECRET.
    pub read_key: Vec<u8>,
    /// A co-publisher's 32-byte PublisherGroup id; empty for every other
    /// role.
    pub publisher_group_id: Vec<u8>,
    /// A co-publisher's record of the channel owner's 32-byte account key
    /// (ADR-0090 section 2.3); empty for every other role. A co-publisher
    /// device accepts a PublisherGroup Welcome only from a device of this
    /// key (`publisherGroupJoinFromWelcome(..., ownerAccountKey:)`).
    pub owner_account_key: Vec<u8>,
    /// The channel name the list falls back to, or empty.
    pub name: String,
    /// The channel's `@name` without the `@`, or empty.
    pub handle: String,
}

impl std::fmt::Debug for ChannelSubscriptionRegister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The subscriber secret and the read key read the channel: left out.
        f.debug_struct("ChannelSubscriptionRegister")
            .field("hlc", &self.hlc)
            .field("subscribed", &self.subscribed)
            .field("role", &self.role)
            .field("has_read_key", &!self.read_key.is_empty())
            .field("publisher_group_id_len", &self.publisher_group_id.len())
            .field("owner_account_key_len", &self.owner_account_key.len())
            .finish_non_exhaustive()
    }
}

fn channel_subscription_register(
    s: Stamped<core::ChannelSubscriptionEntry>,
) -> ChannelSubscriptionRegister {
    let channel_id = s.value.channel_id.to_vec();
    match s.value.subscribed {
        Some(v) => ChannelSubscriptionRegister {
            hlc: s.hlc,
            channel_id,
            subscribed: true,
            role: v.role.into(),
            subscriber_secret: v.subscriber_secret.to_vec(),
            read_key: v.read_key.map(|k| k.to_vec()).unwrap_or_default(),
            publisher_group_id: v.publisher_group_id.map(|g| g.to_vec()).unwrap_or_default(),
            owner_account_key: v.owner_account_key.map(|k| k.to_vec()).unwrap_or_default(),
            name: v.name,
            handle: v.handle,
        },
        None => ChannelSubscriptionRegister {
            hlc: s.hlc,
            channel_id,
            subscribed: false,
            role: ChannelSubscriptionRole::Subscriber,
            subscriber_secret: Vec::new(),
            read_key: Vec::new(),
            publisher_group_id: Vec::new(),
            owner_account_key: Vec::new(),
            name: String::new(),
            handle: String::new(),
        },
    }
}

/// An optional 32-byte field: empty is none.
fn optional32(bytes: &[u8], what: &str) -> Result<Option<[u8; 32]>, FfiError> {
    if bytes.is_empty() {
        Ok(None)
    } else {
        fixed::<32>(bytes, what).map(Some)
    }
}

fn channel_subscription_refusal(e: core::ChannelSubscriptionEnvelopeError) -> FfiError {
    FfiError::SyncedState(format!("{}: {e}", e.code()))
}

fn link_record_refusal(e: core::LinkRecordEnvelopeError) -> FfiError {
    FfiError::SyncedState(format!("{}: {e}", e.code()))
}

fn own_username_refusal(e: core::OwnUsernameEnvelopeError) -> FfiError {
    FfiError::SyncedState(format!("{}: {e}", e.code()))
}

fn recovery_email_refusal(e: core::RecoveryEmailEnvelopeError) -> FfiError {
    FfiError::SyncedState(format!("{}: {e}", e.code()))
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

    /// Record that `successor_group_id` continues the conversation
    /// `root_conversation_id` (ADR-0073 R.5), stamped `hlc`. **Write-once**:
    /// when a register is already held for this successor nothing is
    /// written and `false` is returned, whatever root it names (read it with
    /// [`Self::lineage_root`]). On `true`, send
    /// [`Self::lineage_envelope_for`] to the account's other devices in a
    /// DeviceGroup service envelope.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] when either id is empty or longer than
    /// [`lineage_group_id_max_len`], or the two are equal.
    pub fn set_lineage(
        &self,
        hlc: u64,
        successor_group_id: Vec<u8>,
        root_conversation_id: Vec<u8>,
    ) -> Result<bool, FfiError> {
        self.with(|s| s.set_lineage(hlc, &successor_group_id, &root_conversation_id))?
            .map_err(|e| FfiError::SyncedState(e.to_string()))
    }

    /// The lineage register held for a successor group, or `None` (the
    /// group is a root, or this device knows no lineage for it).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn lineage_root(
        &self,
        successor_group_id: Vec<u8>,
    ) -> Result<Option<LineageRegister>, FfiError> {
        self.with(|s| s.lineage_root(&successor_group_id).map(lineage_register))
    }

    /// The conversation id a group belongs to: the root its lineage register
    /// names, or `group_id` itself when it holds none.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn conversation_id_of(&self, group_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.with(|s| s.conversation_id_of(&group_id))
    }

    /// Every successor recorded for one conversation, ordered by `(stamp,
    /// successor id)`. The root itself is not listed.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn lineage_successors(
        &self,
        root_conversation_id: Vec<u8>,
    ) -> Result<Vec<LineageRegister>, FfiError> {
        self.with(|s| {
            s.lineage_successors(&root_conversation_id)
                .into_iter()
                .map(lineage_register)
                .collect()
        })
    }

    /// Every lineage register, ascending by successor id.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_lineage(&self) -> Result<Vec<LineageRegister>, FfiError> {
        self.with(|s| s.all_lineage().into_iter().map(lineage_register).collect())
    }

    /// The envelope carrying the one register held for `successor_group_id`
    /// with its original stamp, or `None` when none is held: the live send
    /// after [`Self::set_lineage`] returned `true`
    /// (`DeviceGroup.createServiceEnvelopeMessage`).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn lineage_envelope_for(
        &self,
        successor_group_id: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        self.with(|s| s.lineage_envelope_for(&successor_group_id))
    }

    /// Every held lineage register with its original stamp, as service
    /// envelopes: the catch-up re-send beside [`Self::snapshot_parts`]
    /// (lineage registers are NOT in the snapshot parts). Empty when none
    /// is held.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn lineage_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.lineage_envelopes())
    }

    /// Decode a lineage envelope received in a DeviceGroup service envelope
    /// (route on [`is_lineage_envelope`]) and fold every register in with
    /// its original stamp. Fail closed: a malformed envelope changes
    /// nothing. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `truncated` / `trailing` / `count`
    /// / `notCanonical` / `bounds`.
    pub fn apply_lineage_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_lineage_envelope(&envelope))?
            .map_err(lineage_refusal)
    }

    /// Write the account's recovery email register (ADR-0075, amended
    /// 2026-10-13), stamped `hlc`: an address sets it, `None` (or a blank
    /// string) records that the account has none. The address is normalised
    /// here with the one rule (`normalizeRecoveryEmail`), so any spelling
    /// the person typed may be passed.
    ///
    /// An ordinary last-writer-wins write, NOT write-once: `true` when the
    /// register now holds this value, in which case send
    /// [`Self::recovery_email_envelope`] to the account's other devices in a
    /// DeviceGroup service envelope
    /// (`DeviceGroup.createServiceEnvelopeMessage`); `false` when what is
    /// held is newer or identical and nothing changed. A removal is kept
    /// with its stamp, so a stale device cannot bring the address back.
    ///
    /// The address is personal data: it stays on the account's devices and
    /// in the sealed backup, and must not be logged.
    ///
    /// # Errors
    ///
    /// [`FfiError::RecoveryEmail`] `"<code>: <reason>"` (the codes of
    /// `normalizeRecoveryEmail`) when the address is refused.
    pub fn set_recovery_email(&self, hlc: u64, address: Option<String>) -> Result<bool, FfiError> {
        let normalised = match address.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(
                lakko_mnemonic::recovery_email::normalize_recovery_email(raw)
                    .map_err(|e| FfiError::RecoveryEmail(format!("{}: {e}", e.code())))?,
            ),
        };
        self.with(|s| s.set_recovery_email(hlc, normalised.as_deref()))?
            .map_err(|e| FfiError::SyncedState(e.to_string()))
    }

    /// The account's recovery email address, or `None` when the register
    /// was never written or holds a removal.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn recovery_email(&self) -> Result<Option<String>, FfiError> {
        self.with(|s| s.recovery_email())
    }

    /// The recovery email register as held, a removal included
    /// (`address == None` with its stamp); `None` when it was never written.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn recovery_email_register(&self) -> Result<Option<RecoveryEmailRegister>, FfiError> {
        self.with(|s| s.recovery_email_register().map(recovery_email_register))
    }

    /// The service envelope carrying the held register with its original
    /// stamp, a removal included: the live send after
    /// [`Self::set_recovery_email`] returned `true`, and the catch-up
    /// re-send beside [`Self::snapshot_parts`] and
    /// [`Self::lineage_envelopes`] (the register is NOT in the snapshot
    /// parts). `None` when the register was never written.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn recovery_email_envelope(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.with(|s| s.recovery_email_envelope())
    }

    /// Decode a recovery-email envelope received in a DeviceGroup service
    /// envelope (route on [`is_recovery_email_envelope`]) and fold the
    /// register in with its original stamp. Fail closed: a malformed
    /// envelope changes nothing. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `truncated` / `trailing` /
    /// `notUtf8` / `bounds`.
    pub fn apply_recovery_email_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_recovery_email_envelope(&envelope))?
            .map_err(recovery_email_refusal)
    }

    /// Write the account's own-username register (ADR-0076 section 3),
    /// stamped `hlc`: a username sets it, `None` (or a blank string) records
    /// that the account has none. The username is normalised and checked
    /// here with the one rule (`validateUsername`), so `"@Bob"` may be
    /// passed and `"bob"` is stored.
    ///
    /// An ordinary last-writer-wins write, NOT write-once: `true` when the
    /// register now holds this value, in which case send
    /// [`Self::own_username_envelope`] to the account's other devices in a
    /// DeviceGroup service envelope
    /// (`DeviceGroup.createServiceEnvelopeMessage`); `false` when what is
    /// held is newer or identical and nothing changed. A "none" is kept with
    /// its stamp, so a stale device cannot bring an old name back.
    ///
    /// # Errors
    ///
    /// [`FfiError::UsernameEmpty`] / [`FfiError::UsernameTooShort`] /
    /// [`FfiError::UsernameTooLong`] / [`FfiError::UsernameInvalidCharacters`],
    /// as for `validateUsername`.
    pub fn set_own_username(&self, hlc: u64, username: Option<String>) -> Result<bool, FfiError> {
        let canonical = match username.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(lakko_discovery_client::validate_username(raw)?),
        };
        self.with(|s| s.set_own_username(hlc, canonical.as_deref()))?
            .map_err(|e| FfiError::SyncedState(e.to_string()))
    }

    /// The account's own username, or `None` when the register was never
    /// written or holds "none".
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn own_username(&self) -> Result<Option<String>, FfiError> {
        self.with(|s| s.own_username())
    }

    /// The own-username register as held, a "none" included
    /// (`username == None` with its stamp); `None` when it was never
    /// written.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn own_username_register(&self) -> Result<Option<OwnUsernameRegister>, FfiError> {
        self.with(|s| s.own_username_register().map(own_username_register))
    }

    /// The service envelope carrying the held register with its original
    /// stamp, a "none" included: the live send after
    /// [`Self::set_own_username`] returned `true`, and the catch-up re-send
    /// beside [`Self::snapshot_parts`], [`Self::lineage_envelopes`] and
    /// [`Self::recovery_email_envelope`] (the register is NOT in the
    /// snapshot parts). `None` when the register was never written.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn own_username_envelope(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.with(|s| s.own_username_envelope())
    }

    /// Decode an own-username envelope received in a DeviceGroup service
    /// envelope (route on [`is_own_username_envelope`]) and fold the
    /// register in with its original stamp. Fail closed: a malformed
    /// envelope changes nothing. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `truncated` / `trailing` /
    /// `notUtf8` / `bounds`.
    pub fn apply_own_username_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_own_username_envelope(&envelope))?
            .map_err(own_username_refusal)
    }

    /// Record that the person has seen the "a device was added to your
    /// account" warning (identity-recovery 17.5) for each of `device_keys`
    /// (32-byte device signature keys), stamped `hlc`, at wall-clock
    /// `now_ms`. Write-once per key. Returns the envelopes to send to the
    /// account's other devices in DeviceGroup service envelopes, carrying
    /// exactly what this call wrote (empty when nothing was new).
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"bounds: ..."` when a key is not 32 bytes;
    /// [`FfiError::LockPoisoned`].
    pub fn acknowledge_devices(
        &self,
        hlc: u64,
        device_keys: Vec<Vec<u8>>,
        now_ms: u64,
    ) -> Result<Vec<Vec<u8>>, FfiError> {
        let keys = device_keys
            .iter()
            .map(|k| fixed::<{ core::DEVICE_ACK_KEY_LEN }>(k, "device key"))
            .collect::<Result<Vec<_>, _>>()?;
        self.with(|s| s.acknowledge_devices(hlc, &keys, now_ms))
    }

    /// Whether the added-device warning for `device_key` was seen on some
    /// device of the account. A key of another length is never acknowledged.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn is_device_acknowledged(&self, device_key: Vec<u8>) -> Result<bool, FfiError> {
        let Ok(key) = <[u8; core::DEVICE_ACK_KEY_LEN]>::try_from(device_key.as_slice()) else {
            return Ok(false);
        };
        self.with(|s| s.is_device_acknowledged(&key))
    }

    /// Every acknowledged device key, ascending.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn acknowledged_devices(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| {
            s.acknowledged_devices()
                .iter()
                .map(|k| k.to_vec())
                .collect()
        })
    }

    /// Every held acknowledgement as envelopes: the re-send a device owes
    /// when a live send did not go out.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn device_ack_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.device_ack_envelopes())
    }

    /// Decode a device-acknowledgement envelope received in a DeviceGroup
    /// service envelope (route on [`is_device_ack_envelope`]) and fold every
    /// register in. Fail closed. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `count` / `truncated` /
    /// `notCanonical` / `trailing`.
    pub fn apply_device_ack_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_device_ack_envelope(&envelope))?
            .map_err(|e| FfiError::SyncedState(format!("{}: {e}", e.code())))
    }

    // -----------------------------------------------------------------
    // Quick reaction (UX reactions-and-emoji section 6)
    // -----------------------------------------------------------------

    /// Set the account's quick reaction, stamped `hlc`. `true` when the
    /// register now holds it: send [`Self::quick_reaction_envelope`] to the
    /// account's other devices in a DeviceGroup service envelope. Offer only
    /// what `isSingleEmoji` accepts.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an empty or over-long value;
    /// [`FfiError::LockPoisoned`].
    pub fn set_quick_reaction(&self, hlc: u64, emoji: String) -> Result<bool, FfiError> {
        self.with(|s| s.set_quick_reaction(hlc, &emoji))?
            .map_err(synced_err)
    }

    /// The account's quick reaction, or `None` when never chosen (then use
    /// `defaultQuickReaction()`).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn quick_reaction(&self) -> Result<Option<String>, FfiError> {
        self.with(|s| s.quick_reaction())
    }

    /// The envelope carrying the held quick reaction with its original stamp
    /// (the live send and the catch-up re-send); `None` when never written.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn quick_reaction_envelope(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.with(|s| s.quick_reaction_envelope())
    }

    /// Fold in a quick-reaction envelope (route on
    /// [`is_quick_reaction_envelope`]). Fail closed. Returns whether
    /// anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"`.
    pub fn apply_quick_reaction_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_quick_reaction_envelope(&envelope))?
            .map_err(|e| FfiError::SyncedState(format!("{}: {e}", e.code())))
    }

    // -----------------------------------------------------------------
    // Own channel reactions and comments (ADR-0087 section 13a)
    // -----------------------------------------------------------------

    /// Record that this account reacted (`reacted`) or took the reaction
    /// off, `emoji` under the post at `post_cursor` of `channel_id` (posted at
    /// `post_at_ms`, which drives the 30-day retention). Returns the envelope
    /// to send to the account's other devices, or `None` when nothing
    /// changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"bounds: ..."` for a fourth own reaction on
    /// one post or a bad argument; [`FfiError::LockPoisoned`].
    pub fn set_channel_reaction(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        post_cursor: u64,
        emoji: String,
        reacted: bool,
        post_at_ms: u64,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| {
            s.set_channel_reaction(hlc, channel, post_cursor, &emoji, reacted, post_at_ms)
        })?
        .map_err(synced_err)
    }

    /// Whether this account holds `emoji` on the post.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad channel id; [`FfiError::LockPoisoned`].
    pub fn has_channel_reaction(
        &self,
        channel_id: Vec<u8>,
        post_cursor: u64,
        emoji: String,
    ) -> Result<bool, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| s.has_channel_reaction(&channel, post_cursor, &emoji))
    }

    /// The emoji this account holds on the post itself (not its comments).
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad channel id; [`FfiError::LockPoisoned`].
    pub fn channel_reactions_on(
        &self,
        channel_id: Vec<u8>,
        post_cursor: u64,
    ) -> Result<Vec<String>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| s.channel_reactions_on(&channel, post_cursor))
    }

    /// The same as [`Self::set_channel_reaction`] for a reaction on the
    /// comment at `comment_cursor` under the post at `post_cursor` (ADR-0087
    /// phase B). `post_at_ms` is the POST's time: a comment lives as long as
    /// its post. Three reactions a comment, counted apart from the post.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"bounds: ..."` for a fourth own reaction on
    /// one comment or a bad argument; [`FfiError::LockPoisoned`].
    #[allow(clippy::too_many_arguments)]
    pub fn set_comment_reaction(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_cursor: u64,
        emoji: String,
        reacted: bool,
        post_at_ms: u64,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| {
            s.set_comment_reaction(
                hlc,
                channel,
                post_cursor,
                comment_cursor,
                &emoji,
                reacted,
                post_at_ms,
            )
        })?
        .map_err(synced_err)
    }

    /// Whether this account holds `emoji` on the comment.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad channel id; [`FfiError::LockPoisoned`].
    pub fn has_comment_reaction(
        &self,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_cursor: u64,
        emoji: String,
    ) -> Result<bool, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| s.has_comment_reaction(&channel, post_cursor, comment_cursor, &emoji))
    }

    /// The emoji this account holds on the comment.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad channel id; [`FfiError::LockPoisoned`].
    pub fn comment_reactions_on(
        &self,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<Vec<String>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| s.comment_reactions_on(&channel, post_cursor, comment_cursor))
    }

    /// Every held own channel reaction as envelopes (the catch-up re-send).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn channel_reaction_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.channel_reaction_envelopes())
    }

    /// Fold in an own-channel-reaction envelope (route on
    /// [`is_channel_reaction_envelope`]). Fail closed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"`.
    pub fn apply_channel_reaction_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_channel_reaction_envelope(&envelope))?
            .map_err(|e| FfiError::SyncedState(format!("{}: {e}", e.code())))
    }

    /// Record a comment this account wrote: its 16-byte `comment_nonce`
    /// (what derives its write key) under the post. Returns the envelope to
    /// send, or `None` when it was already held or the client cap is reached.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad argument; [`FfiError::LockPoisoned`].
    pub fn record_channel_comment(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_nonce: Vec<u8>,
        post_at_ms: u64,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        let nonce = fixed::<{ core::CHANNEL_COMMENT_NONCE_LEN }>(&comment_nonce, "comment nonce")?;
        self.with(|s| s.record_channel_comment(hlc, channel, post_cursor, nonce, post_at_ms))
    }

    /// Record that this account deleted one of its comments. Returns the
    /// envelope to send, or `None` when it was already deleted.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad argument; [`FfiError::LockPoisoned`].
    pub fn delete_channel_comment(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_nonce: Vec<u8>,
        post_at_ms: u64,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        let nonce = fixed::<{ core::CHANNEL_COMMENT_NONCE_LEN }>(&comment_nonce, "comment nonce")?;
        self.with(|s| s.delete_channel_comment(hlc, channel, post_cursor, nonce, post_at_ms))
    }

    /// The nonces of this account's live comments under the post.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for a bad channel id; [`FfiError::LockPoisoned`].
    pub fn channel_comments_on(
        &self,
        channel_id: Vec<u8>,
        post_cursor: u64,
    ) -> Result<Vec<Vec<u8>>, FfiError> {
        let channel =
            fixed::<{ core::CHANNEL_REACTION_CHANNEL_ID_LEN }>(&channel_id, "channel id")?;
        self.with(|s| {
            s.channel_comments_on(&channel, post_cursor)
                .iter()
                .map(|n| n.to_vec())
                .collect()
        })
    }

    /// Every held own comment record as envelopes (the catch-up re-send).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn channel_comment_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.channel_comment_envelopes())
    }

    /// Fold in an own-channel-comment envelope (route on
    /// [`is_channel_comment_envelope`]). Fail closed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"`.
    pub fn apply_channel_comment_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_channel_comment_envelope(&envelope))?
            .map_err(|e| FfiError::SyncedState(format!("{}: {e}", e.code())))
    }

    /// Drop own channel reactions and comments whose post is past the 30-day
    /// retention at `now_ms`. Returns how many registers were dropped.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn prune_channel_activity(&self, now_ms: u64) -> Result<u64, FfiError> {
        self.with(|s| u64::try_from(s.prune_channel_activity(now_ms)).unwrap_or(u64::MAX))
    }

    /// Write one of the account's invite links (ADR-0079 section 4), stamped
    /// `hlc`: at mint, and for any change. `true` when written; then send
    /// [`Self::link_record_envelope_for`] in a DeviceGroup service envelope.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"bounds: ..."` for a field out of bounds.
    pub fn set_link_record(&self, hlc: u64, record: LinkRecord) -> Result<bool, FfiError> {
        let entry = link_record_entry(record)?;
        self.with(|s| s.set_link_record(hlc, entry))?
            .map_err(|e| FfiError::SyncedState(format!("bounds: {e}")))
    }

    /// The record held for a link id, or `None`.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`]; [`FfiError::SyncedState`] for an id that
    /// is not 16 bytes.
    pub fn link_record(&self, link_id: Vec<u8>) -> Result<Option<LinkRecord>, FfiError> {
        let id = fixed::<16>(&link_id, "link id")?;
        self.with(|s| s.link_record(&id).map(link_record))
    }

    /// Every link record (revoked ones included), ascending by link id.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_link_records(&self) -> Result<Vec<LinkRecord>, FfiError> {
        self.with(|s| s.all_link_records().into_iter().map(link_record).collect())
    }

    /// Mark a link revoked (final). `true` when the register changed: send
    /// [`Self::link_record_envelope_for`].
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn revoke_link_record(&self, hlc: u64, link_id: Vec<u8>) -> Result<bool, FfiError> {
        let id = fixed::<16>(&link_id, "link id")?;
        self.with(|s| s.revoke_link_record(hlc, &id))?
            .map_err(|e| FfiError::SyncedState(format!("bounds: {e}")))
    }

    /// Mark a link copied (2026-10-26): it is no longer editable, on this
    /// device and, once [`Self::link_record_envelope_for`] is sent, on every
    /// device of the account. `true` when the register changed (send it).
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id of the wrong length.
    pub fn mark_link_shared(&self, hlc: u64, link_id: Vec<u8>) -> Result<bool, FfiError> {
        let id = fixed::<16>(&link_id, "link id")?;
        self.with(|s| s.mark_link_shared(hlc, &id))?
            .map_err(|e| FfiError::SyncedState(format!("bounds: {e}")))
    }

    /// Count one request accepted through a link from `requester_key` (the
    /// request's 32-byte `from_pubkey`). `true` when the register changed:
    /// send [`Self::link_record_envelope_for`].
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id or key of the wrong length.
    pub fn record_link_use(
        &self,
        hlc: u64,
        link_id: Vec<u8>,
        requester_key: Vec<u8>,
    ) -> Result<bool, FfiError> {
        let id = fixed::<16>(&link_id, "link id")?;
        let key = fixed::<32>(&requester_key, "requester key")?;
        self.with(|s| s.record_link_use(hlc, &id, &key))?
            .map_err(|e| FfiError::SyncedState(format!("bounds: {e}")))
    }

    /// The one rule for a contact request carrying a link answer (ADR-0079
    /// section 3 as amended 2026-10-19): compare the answer with the held
    /// record (the secret in constant time). `pressed_at_ms` is the time the
    /// Delivery Service stamped on the request row: the link's expiry is
    /// judged at that moment, its revocation and use count now. An answer of
    /// the wrong width is `Dead`.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn link_answer_verdict(
        &self,
        link_id: Vec<u8>,
        link_secret: Vec<u8>,
        requester_key: Vec<u8>,
        pressed_at_ms: u64,
    ) -> Result<LinkAnswerVerdict, FfiError> {
        let (Ok(id), Ok(secret), Ok(key)) = (
            <[u8; 16]>::try_from(link_id.as_slice()),
            <[u8; 12]>::try_from(link_secret.as_slice()),
            <[u8; 32]>::try_from(requester_key.as_slice()),
        ) else {
            return Ok(LinkAnswerVerdict::Dead);
        };
        self.with(|s| {
            s.link_answer_verdict(&id, &secret, &key, pressed_at_ms)
                .into()
        })
    }

    /// The service envelope carrying one held link record with its original
    /// stamp, or `None`.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn link_record_envelope_for(&self, link_id: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
        let id = fixed::<16>(&link_id, "link id")?;
        self.with(|s| s.link_record_envelope_for(&id))
    }

    /// Every held link record with its original stamp, as service envelopes:
    /// the catch-up re-send beside [`Self::snapshot_parts`] (link records are
    /// NOT in the snapshot parts).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn link_record_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.link_record_envelopes())
    }

    /// Decode a link-record envelope (route on [`is_link_record_envelope`])
    /// and fold every register in. Fail closed. Returns whether anything
    /// changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `truncated` / `trailing` / `count`
    /// / `notCanonical` / `entry`.
    pub fn apply_link_record_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_link_record_envelope(&envelope))?
            .map_err(link_record_refusal)
    }

    /// Record that the account is subscribed to `channel_id` (2026-11-13):
    /// at a subscribe, and whenever a field changes (a read key learned, the
    /// channel's name or `@name` seen, a co-publisher joined). `read_key`
    /// and `publisher_group_id` are empty for none; `publisher_group_id` is
    /// required for a `CoPublisher` and refused for any other role, and so is
    /// `owner_account_key` (ADR-0090 section 2.3): the 32-byte account key of
    /// the channel's owner, which the device that joined as co-publisher
    /// takes from the issuer of the `/g/` capability it verified; empty for an
    /// owner (its own key derives from the account seed) and a subscriber.
    /// Stamped
    /// at `max(hlc, held + 1)`. `true` when written: then send
    /// [`Self::channel_subscription_envelope_for`] in a DeviceGroup service
    /// envelope. `false` when the identical subscription is already held.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"bounds: ..."` for a field out of bounds,
    /// or for a new channel when [`channel_subscriptions_max_client`] are
    /// held live.
    #[allow(clippy::too_many_arguments)]
    pub fn set_channel_subscription(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        role: ChannelSubscriptionRole,
        subscriber_secret: Vec<u8>,
        read_key: Vec<u8>,
        publisher_group_id: Vec<u8>,
        owner_account_key: Vec<u8>,
        name: String,
        handle: String,
    ) -> Result<bool, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        let subscribed = core::ChannelSubscribed {
            role: role.into(),
            subscriber_secret: fixed::<32>(&subscriber_secret, "subscriber secret")?,
            read_key: optional32(&read_key, "read key")?,
            publisher_group_id: optional32(&publisher_group_id, "publisher group id")?,
            owner_account_key: optional32(&owner_account_key, "owner account key")?,
            name,
            handle,
        };
        self.with(|s| s.set_channel_subscription(hlc, &id, subscribed))?
            .map_err(|e| FfiError::SyncedState(format!("bounds: {e}")))
    }

    /// Record that the account left `channel_id` (the tombstone). `true` when
    /// written (send [`Self::channel_subscription_envelope_for`]); `false`
    /// when no subscription is held or it was already left.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn leave_channel_subscription(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
    ) -> Result<bool, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| s.leave_channel_subscription(hlc, &id))
    }

    /// The register held for a channel (a subscription or a leave), or
    /// `None`.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn channel_subscription(
        &self,
        channel_id: Vec<u8>,
    ) -> Result<Option<ChannelSubscriptionRegister>, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| {
            s.channel_subscription(&id)
                .map(channel_subscription_register)
        })
    }

    /// `true` while the account is subscribed to `channel_id`.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn is_channel_subscribed(&self, channel_id: Vec<u8>) -> Result<bool, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| s.is_channel_subscribed(&id))
    }

    /// Every channel-subscription register (leaves included), ascending by
    /// channel id: the list a device reconciles its channels against.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn all_channel_subscriptions(&self) -> Result<Vec<ChannelSubscriptionRegister>, FfiError> {
        self.with(|s| {
            s.all_channel_subscriptions()
                .into_iter()
                .map(channel_subscription_register)
                .collect()
        })
    }

    /// The service envelope carrying one held channel-subscription register
    /// with its original stamp, or `None`.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id that is not 16 bytes.
    pub fn channel_subscription_envelope_for(
        &self,
        channel_id: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| s.channel_subscription_envelope_for(&id))
    }

    /// Every held channel-subscription register with its original stamp, as
    /// service envelopes: the catch-up re-send on DeviceGroup growth beside
    /// [`Self::snapshot_parts`] (they are NOT in the snapshot parts).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn channel_subscription_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.channel_subscription_envelopes())
    }

    /// Decode a channel-subscription envelope (route on
    /// [`is_channel_subscription_envelope`]) and fold every register in.
    /// Fail closed. Returns whether anything changed; then read
    /// [`Self::all_channel_subscriptions`] and reconcile.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `count` / `truncated` /
    /// `notCanonical` / `trailing`.
    pub fn apply_channel_subscription_envelope(&self, envelope: Vec<u8>) -> Result<bool, FfiError> {
        self.with(|s| s.apply_channel_subscription_envelope(&envelope))?
            .map_err(channel_subscription_refusal)
    }

    /// Record a private channel's admission seed of `admission_version`, the
    /// owner account's own copy (ADR-0090 open question 4). Write-once per
    /// seed: `false` when this seed is held; `true` when written, then send
    /// [`Self::channel_admission_seed_envelope_for`] in a DeviceGroup service
    /// envelope. Another seed of the same version (two owner devices minting
    /// it at once) is kept beside it.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id not 16 bytes or a seed not 32.
    pub fn set_channel_admission_seed(
        &self,
        hlc: u64,
        channel_id: Vec<u8>,
        admission_version: u16,
        seed: Vec<u8>,
    ) -> Result<bool, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        let seed = fixed::<32>(&seed, "admission seed")?;
        self.with(|s| s.set_channel_admission_seed(hlc, &id, admission_version, &seed))
    }

    /// Every seed held for `channel_id` at `admission_version` (more than one
    /// when two owner devices minted that version at once). A ticket of that
    /// version is checked against EACH of them; the row is admitted when it
    /// opens under any.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id not 16 bytes.
    pub fn channel_admission_seeds_for(
        &self,
        channel_id: Vec<u8>,
        admission_version: u16,
    ) -> Result<Vec<Vec<u8>>, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| {
            s.channel_admission_seeds_for(&id, admission_version)
                .iter()
                .map(|seed| seed.to_vec())
                .collect()
        })
    }

    /// Every admission seed held for one channel, ascending by version (a
    /// version may appear more than once): what an owner device opens
    /// tickets with (`openAdmissionTicket`).
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id not 16 bytes.
    pub fn channel_admission_seeds(
        &self,
        channel_id: Vec<u8>,
    ) -> Result<Vec<ChannelAdmissionSeedView>, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        self.with(|s| {
            s.channel_admission_seeds(&id)
                .into_iter()
                .map(|(admission_version, seed)| ChannelAdmissionSeedView {
                    admission_version,
                    seed: seed.to_vec(),
                })
                .collect()
        })
    }

    /// The service envelope carrying the held register of this seed with its
    /// original stamp, or `None` when it is not held.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] for an id not 16 bytes or a seed not 32.
    pub fn channel_admission_seed_envelope_for(
        &self,
        channel_id: Vec<u8>,
        admission_version: u16,
        seed: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        let id = fixed::<16>(&channel_id, "channel id")?;
        let seed = fixed::<32>(&seed, "admission seed")?;
        self.with(|s| s.channel_admission_seed_envelope_for(&id, admission_version, &seed))
    }

    /// Every held admission-seed register as service envelopes: the
    /// catch-up re-send on DeviceGroup growth (NOT in the snapshot parts).
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn channel_admission_seed_envelopes(&self) -> Result<Vec<Vec<u8>>, FfiError> {
        self.with(|s| s.channel_admission_seed_envelopes())
    }

    /// Decode an admission-seed envelope (route on
    /// [`is_channel_admission_seed_envelope`]) and fold every register in.
    /// Fail closed. Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] `"<code>: <reason>"` with code
    /// `wrongDomain` / `unknownVersion` / `count` / `truncated` /
    /// `notCanonical` / `trailing`.
    pub fn apply_channel_admission_seed_envelope(
        &self,
        envelope: Vec<u8>,
    ) -> Result<bool, FfiError> {
        self.with(|s| s.apply_channel_admission_seed_envelope(&envelope))?
            .map_err(|e| FfiError::SyncedState(format!("{}: {e}", e.code())))
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
                    sent_at_ms: v.snapshot.sent_at_ms,
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

    /// The register to write now that this account's membership has ended,
    /// or `None` when there is nothing to tell the other devices (the
    /// register already says so, the account has rejoined at a later epoch, or
    /// [`conversation_ends_max_client`] are held). `epoch` is the epoch the
    /// group froze at, `0` for a channel. `None` never stops the end itself:
    /// the device still wipes the conversation.
    ///
    /// Does not fold anything: put the register in a change set.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] when the conversation id is empty or longer
    /// than 64 bytes; [`FfiError::LockPoisoned`].
    pub fn compose_conversation_ended(
        &self,
        hlc: u64,
        conversation: FolderConversationRef,
        epoch: u64,
    ) -> Result<Option<ConversationEndRegister>, FfiError> {
        let conversation: ConversationRef = conversation.into();
        self.with(|s| s.compose_conversation_ended(hlc, &conversation, epoch))?
            .map(|r| r.map(ConversationEndRegister::from_core))
            .map_err(synced_err)
    }

    /// The register to write now that this account has joined a conversation,
    /// or `None` when no end is held to clear: a first join writes nothing.
    /// `epoch` is the epoch the Welcome joined at (`Group.epoch()` right after
    /// the join), `0` for a channel.
    ///
    /// Does not fold anything: put the register in a change set.
    ///
    /// # Errors
    ///
    /// [`FfiError::SyncedState`] when the conversation id is empty or longer
    /// than 64 bytes; [`FfiError::LockPoisoned`].
    pub fn compose_conversation_joined(
        &self,
        hlc: u64,
        conversation: FolderConversationRef,
        epoch: u64,
    ) -> Result<Option<ConversationEndRegister>, FfiError> {
        let conversation: ConversationRef = conversation.into();
        self.with(|s| s.compose_conversation_joined(hlc, &conversation, epoch))?
            .map(|r| r.map(ConversationEndRegister::from_core))
            .map_err(synced_err)
    }

    /// The register held for a conversation, a "joined" included. `None` when
    /// no end ever reached it.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn conversation_end(
        &self,
        conversation: FolderConversationRef,
    ) -> Result<Option<ConversationEndRegister>, FfiError> {
        let conversation: ConversationRef = conversation.into();
        self.with(|s| {
            s.conversation_end(&conversation)
                .map(ConversationEndRegister::from_core)
        })
    }

    /// Whether this account's membership of a conversation has ended. `false`
    /// for one never ended and for one joined again since.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn is_conversation_ended(
        &self,
        conversation: FolderConversationRef,
    ) -> Result<bool, FfiError> {
        let conversation: ConversationRef = conversation.into();
        self.with(|s| s.is_conversation_ended(&conversation))
    }

    /// Every conversation whose register says ended, by `(kind, id)`.
    ///
    /// # Errors
    ///
    /// [`FfiError::LockPoisoned`].
    pub fn ended_conversations(&self) -> Result<Vec<ConversationEndRegister>, FfiError> {
        self.with(|s| {
            s.ended_conversations()
                .into_iter()
                .map(ConversationEndRegister::from_core)
                .collect()
        })
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

/// `true` when `bytes` open with the lineage envelope domain
/// (`lakko/sync/lineage/v1`): the routing question for a DeviceGroup service
/// envelope, asked before the `lakko-contacts` dispatcher. Says nothing
/// about validity; [`SyncedState::apply_lineage_envelope`] decides that.
#[uniffi::export]
#[must_use]
pub fn is_lineage_envelope(bytes: Vec<u8>) -> bool {
    core::is_lineage_envelope(&bytes)
}

/// Longest lineage envelope [`SyncedState::lineage_envelopes`] produces;
/// below `deviceGroupServiceEnvelopeMaxLen()`.
#[uniffi::export]
#[must_use]
pub fn lineage_envelope_max_len() -> u32 {
    u32::try_from(core::LINEAGE_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Longest group id a lineage register accepts (each of the two): 64.
#[uniffi::export]
#[must_use]
pub fn lineage_group_id_max_len() -> u32 {
    u32::try_from(core::LINEAGE_GROUP_ID_MAX_LEN).unwrap_or(u32::MAX)
}

/// `true` when `bytes` open with the recovery-email envelope domain
/// (`lakko/sync/recovery-email/v1`): the routing question for a DeviceGroup
/// service envelope, asked before the `lakko-contacts` dispatcher. Says
/// nothing about validity; [`SyncedState::apply_recovery_email_envelope`]
/// decides that.
/// Whether `bytes` is a quick-reaction envelope (route a DeviceGroup
/// service envelope to [`SyncedState::apply_quick_reaction_envelope`]).
#[uniffi::export]
#[must_use]
pub fn is_quick_reaction_envelope(bytes: Vec<u8>) -> bool {
    core::is_quick_reaction_envelope(&bytes)
}

/// Whether `bytes` is an own-channel-reaction envelope.
#[uniffi::export]
#[must_use]
pub fn is_channel_reaction_envelope(bytes: Vec<u8>) -> bool {
    core::is_channel_reaction_envelope(&bytes)
}

/// Whether `bytes` is an own-channel-comment envelope.
#[uniffi::export]
#[must_use]
pub fn is_channel_comment_envelope(bytes: Vec<u8>) -> bool {
    core::is_channel_comment_envelope(&bytes)
}

/// The most own reactions one post takes (3).
#[uniffi::export]
#[must_use]
pub fn channel_reactions_per_post_max() -> u32 {
    u32::try_from(core::CHANNEL_REACTIONS_PER_POST_MAX).unwrap_or(u32::MAX)
}

#[uniffi::export]
#[must_use]
pub fn is_recovery_email_envelope(bytes: Vec<u8>) -> bool {
    core::is_recovery_email_envelope(&bytes)
}

/// Longest recovery-email envelope [`SyncedState::recovery_email_envelope`]
/// produces (293); below `deviceGroupServiceEnvelopeMaxLen()`.
#[uniffi::export]
#[must_use]
pub fn recovery_email_envelope_max_len() -> u32 {
    u32::try_from(core::RECOVERY_EMAIL_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// `true` when `bytes` open with the device-acknowledgement envelope domain
/// (`lakko/sync/device-ack/v1`): the routing question for a DeviceGroup
/// service envelope, asked before the `lakko-contacts` dispatcher. Says
/// nothing about validity; [`SyncedState::apply_device_ack_envelope`]
/// decides that.
#[uniffi::export]
#[must_use]
pub fn is_device_ack_envelope(bytes: Vec<u8>) -> bool {
    core::is_device_ack_envelope(&bytes)
}

/// `true` when `bytes` open with the own-username envelope domain
/// (`lakko/sync/username/v1`): the routing question for a DeviceGroup
/// service envelope, asked before the `lakko-contacts` dispatcher. Says
/// nothing about validity; [`SyncedState::apply_own_username_envelope`]
/// decides that.
#[uniffi::export]
#[must_use]
pub fn is_own_username_envelope(bytes: Vec<u8>) -> bool {
    core::is_own_username_envelope(&bytes)
}

/// `true` when `bytes` open with the link-record envelope domain
/// (`lakko/sync/link-record/v1`, ADR-0079): the routing question for a
/// DeviceGroup service envelope, asked before the `lakko-contacts`
/// dispatcher.
#[uniffi::export]
#[must_use]
pub fn is_link_record_envelope(bytes: Vec<u8>) -> bool {
    core::is_link_record_envelope(&bytes)
}

/// One admission seed of a channel (ADR-0090 open question 4). SECRET: the
/// seed opens the channel's admission tickets of that version.
#[derive(uniffi::Record, Clone)]
pub struct ChannelAdmissionSeedView {
    /// The admission key's version.
    pub admission_version: u16,
    /// The 32-byte seed.
    pub seed: Vec<u8>,
}

impl std::fmt::Debug for ChannelAdmissionSeedView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelAdmissionSeedView")
            .field("admission_version", &self.admission_version)
            .finish_non_exhaustive()
    }
}

/// `true` when `bytes` open with the channel admission-seed envelope domain
/// (ADR-0090): route it to `applyChannelAdmissionSeedEnvelope`.
#[uniffi::export]
#[must_use]
pub fn is_channel_admission_seed_envelope(bytes: Vec<u8>) -> bool {
    core::is_channel_admission_seed_envelope(&bytes)
}

/// `true` when `bytes` open with the channel-subscription envelope domain
/// (`lakko/sync/channel-subscription/v1`, 2026-11-13): the routing question
/// for a DeviceGroup service envelope.
#[uniffi::export]
#[must_use]
pub fn is_channel_subscription_envelope(bytes: Vec<u8>) -> bool {
    core::is_channel_subscription_envelope(&bytes)
}

/// Longest channel-subscription envelope
/// [`SyncedState::channel_subscription_envelopes`] produces; below
/// `deviceGroupServiceEnvelopeMaxLen()`.
#[uniffi::export]
#[must_use]
pub fn channel_subscription_envelope_max_len() -> u32 {
    u32::try_from(core::CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// The most live channel subscriptions a client writes (1000; a client-side
/// limit, never a decode bound).
#[uniffi::export]
#[must_use]
pub fn channel_subscriptions_max_client() -> u32 {
    u32::try_from(core::CHANNEL_SUBSCRIPTIONS_MAX_CLIENT).unwrap_or(u32::MAX)
}

/// Longest link-record envelope [`SyncedState::link_record_envelopes`]
/// produces; below `deviceGroupServiceEnvelopeMaxLen()`.
#[uniffi::export]
#[must_use]
pub fn link_record_envelope_max_len() -> u32 {
    u32::try_from(core::LINK_RECORD_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Longest own-username envelope [`SyncedState::own_username_envelope`]
/// produces (65); below `deviceGroupServiceEnvelopeMaxLen()`.
#[uniffi::export]
#[must_use]
pub fn own_username_envelope_max_len() -> u32 {
    u32::try_from(core::OWN_USERNAME_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
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
/// thumbnails. `sent_at_ms` is the message's own time in UNIX millis as
/// this device shows it (`0` when unknown), kept so any device can show the
/// message date on the Saved screen before it holds the message.
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
    sent_at_ms: u64,
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
            sent_at_ms,
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

/// How many conversation-end registers a client writes:
/// [`SyncedState::compose_conversation_ended`] answers `None` for a new
/// conversation past it. A client-side limit, chosen; NOT a decode bound.
#[uniffi::export]
#[must_use]
pub fn conversation_ends_max_client() -> u32 {
    u32::try_from(core::CONVERSATION_ENDS_MAX_CLIENT).unwrap_or(u32::MAX)
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
    fn lineage_is_write_once_travels_by_envelope_and_persists() {
        let a = SyncedState::new();
        assert!(a.set_lineage(5, vec![1; 32], vec![9; 32]).unwrap());
        assert!(!a.set_lineage(8, vec![1; 32], vec![7; 32]).unwrap());
        assert!(a.set_lineage(6, vec![2; 32], vec![9; 32]).unwrap());
        let held = a.lineage_root(vec![1; 32]).unwrap().unwrap();
        assert_eq!(
            (held.hlc, held.root_conversation_id.clone()),
            (5, vec![9; 32])
        );
        assert_eq!(a.conversation_id_of(vec![2; 32]).unwrap(), vec![9; 32]);
        assert_eq!(a.conversation_id_of(vec![9; 32]).unwrap(), vec![9; 32]);
        assert!(a.lineage_root(vec![9; 32]).unwrap().is_none());
        assert_eq!(a.lineage_successors(vec![9; 32]).unwrap().len(), 2);
        assert_eq!(a.all_lineage().unwrap().len(), 2);

        // Live send of one register, then the catch-up of all of them.
        let b = SyncedState::new();
        let live = a.lineage_envelope_for(vec![1; 32]).unwrap().unwrap();
        assert!(is_lineage_envelope(live.clone()));
        assert!(b.apply_lineage_envelope(live.clone()).unwrap());
        assert!(!b.apply_lineage_envelope(live).unwrap());
        for envelope in a.lineage_envelopes().unwrap() {
            b.apply_lineage_envelope(envelope).unwrap();
        }
        assert_eq!(b.all_lineage().unwrap(), a.all_lineage().unwrap());
        assert!(a.lineage_envelope_for(vec![9; 32]).unwrap().is_none());

        // The snapshot parts do not carry it; persistence and merge do.
        assert_eq!(a.snapshot_parts().unwrap()[0].entry_count(), 0);
        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.all_lineage().unwrap(), a.all_lineage().unwrap());
        let merged = SyncedState::new();
        assert!(merged.merge_from(restored).unwrap());
        assert_eq!(merged.to_bytes().unwrap(), a.to_bytes().unwrap());
    }

    #[test]
    fn lineage_refusals_carry_their_code_and_change_nothing() {
        let s = SyncedState::new();
        for (succ, root) in [
            (vec![], vec![1]),
            (vec![1; 65], vec![1]),
            (vec![1], vec![1]),
        ] {
            assert!(matches!(
                s.set_lineage(1, succ, root),
                Err(FfiError::SyncedState(_))
            ));
        }
        s.set_lineage(1, vec![1], vec![2]).unwrap();
        let mut bad = s.lineage_envelope_for(vec![1]).unwrap().unwrap();
        bad.push(0);
        match SyncedState::new().apply_lineage_envelope(bad) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("trailing: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        match s.apply_lineage_envelope(b"lakko/visibility/v1\0".to_vec()) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("wrongDomain: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(!is_lineage_envelope(b"lakko/visibility/v1\0".to_vec()));
        assert_eq!(s.all_lineage().unwrap().len(), 1);
    }

    /// The largest lineage envelope fits the DeviceGroup carrier it rides
    /// (`lakko-sync` does not depend on `lakko-mls`, so the pin lives here).
    #[test]
    fn the_largest_lineage_envelope_fits_a_service_envelope() {
        const { assert!(core::LINEAGE_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN) };
        assert!(lineage_envelope_max_len() <= crate::device_group_service_envelope_max_len());
        assert_eq!(lineage_group_id_max_len(), 64);
    }

    /// ADR-0087 phase B: a channel subscriber link names its channel and
    /// carries its invite seed and the capability to re-share.
    #[test]
    fn a_channel_link_record_carries_its_target_seed_and_capability() {
        let a = SyncedState::new();
        let ledger = vec![0x0D; 6];
        let channel = LinkRecord {
            hlc: 0,
            link_id: group_link_record_id(ledger.clone()).unwrap(),
            link_secret: vec![2; 12],
            kind: LinkRecordKind::Channel,
            ledger_id: ledger,
            revocation_seed: vec![4; 32],
            expiry_ms: 0,
            max_uses: 0,
            uses: 0,
            revoked: false,
            minted_at_ms: 9,
            label: String::new(),
            answered: vec![],
            shared: false,
            editable: false,
            target_id: vec![0xC1; 16],
            invite_seed: vec![0x5E; 32],
            admission_version: 2,
            capability: vec![0xCA; 120],
        };
        assert!(a.set_link_record(5, channel.clone()).unwrap());
        let stored = a.link_record(channel.link_id.clone()).unwrap().unwrap();
        assert_eq!(stored.target_id, vec![0xC1; 16]);
        assert_eq!(stored.invite_seed, vec![0x5E; 32]);
        assert_eq!(stored.admission_version, 2);
        assert!(a
            .set_link_record(
                5,
                LinkRecord {
                    target_id: vec![0xC1; 15],
                    ..channel.clone()
                }
            )
            .is_err());
        assert!(a.revoke_link_record(6, channel.link_id.clone()).unwrap());
        assert!(group_link_record_id(vec![0; 5]).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one end-to-end scenario, read top to bottom
    fn a_channel_subscription_crosses_the_boundary_and_reaches_a_second_device() {
        const {
            assert!(
                core::CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN
            );
        };
        let ch = vec![0xC1; 16];
        let a = SyncedState::new();
        assert!(a
            .set_channel_subscription(
                5,
                ch.clone(),
                ChannelSubscriptionRole::CoPublisher,
                vec![0x5E; 32],
                vec![],
                vec![0x9A; 32],
                vec![0x0E; 32],
                "News".into(),
                "the_news".into(),
            )
            .unwrap());
        assert!(a.is_channel_subscribed(ch.clone()).unwrap());
        let envelope = a
            .channel_subscription_envelope_for(ch.clone())
            .unwrap()
            .unwrap();
        assert!(is_channel_subscription_envelope(envelope.clone()));
        assert!(envelope.len() <= channel_subscription_envelope_max_len() as usize);
        assert_eq!(
            a.channel_subscription_envelopes().unwrap(),
            vec![envelope.clone()]
        );

        let b = SyncedState::new();
        assert!(b
            .apply_channel_subscription_envelope(envelope.clone())
            .unwrap());
        assert!(!b.apply_channel_subscription_envelope(envelope).unwrap());
        let got = b.channel_subscription(ch.clone()).unwrap().unwrap();
        assert!(got.subscribed);
        assert_eq!(got.role, ChannelSubscriptionRole::CoPublisher);
        assert_eq!(got.subscriber_secret, vec![0x5E; 32]);
        assert!(got.read_key.is_empty());
        assert_eq!(got.publisher_group_id, vec![0x9A; 32]);
        assert_eq!(got.owner_account_key, vec![0x0E; 32]);
        assert_eq!(got.handle, "the_news");
        assert!(!format!("{got:?}").contains("94"));
        let back = SyncedState::from_bytes(b.to_bytes().unwrap()).unwrap();
        assert_eq!(back.all_channel_subscriptions().unwrap(), vec![got]);

        // A leave travels the same way and leaves an empty tombstone.
        assert!(a.leave_channel_subscription(6, ch.clone()).unwrap());
        let left = a
            .channel_subscription_envelope_for(ch.clone())
            .unwrap()
            .unwrap();
        assert!(b.apply_channel_subscription_envelope(left).unwrap());
        let gone = b.channel_subscription(ch.clone()).unwrap().unwrap();
        assert!(!gone.subscribed && gone.subscriber_secret.is_empty());
        assert!(!b.is_channel_subscribed(ch.clone()).unwrap());

        // Refusals: a bound, a wrong-length field, a damaged envelope.
        assert!(a
            .set_channel_subscription(
                7,
                ch.clone(),
                ChannelSubscriptionRole::Owner,
                vec![1; 32],
                vec![],
                vec![2; 32],
                vec![],
                String::new(),
                String::new(),
            )
            .is_err());
        // ADR-0090 2.3: a co-publisher without the owner's key, and an owner
        // with one, are refused.
        assert!(a
            .set_channel_subscription(
                7,
                ch.clone(),
                ChannelSubscriptionRole::CoPublisher,
                vec![1; 32],
                vec![],
                vec![2; 32],
                vec![],
                String::new(),
                String::new(),
            )
            .is_err());
        assert!(a
            .set_channel_subscription(
                7,
                ch.clone(),
                ChannelSubscriptionRole::Owner,
                vec![1; 32],
                vec![],
                vec![],
                vec![3; 32],
                String::new(),
                String::new(),
            )
            .is_err());
        assert!(a
            .set_channel_subscription(
                7,
                ch.clone(),
                ChannelSubscriptionRole::Owner,
                vec![1; 31],
                vec![],
                vec![],
                vec![],
                String::new(),
                String::new(),
            )
            .is_err());
        assert!(a.channel_subscription(vec![0; 15]).is_err());
        let err = b
            .apply_channel_subscription_envelope(
                b"lakko/sync/channel-subscription/v1\0\x09".to_vec(),
            )
            .unwrap_err();
        assert!(format!("{err}").contains("unknownVersion"), "{err}");
        assert_eq!(channel_subscriptions_max_client(), 1000);
    }

    #[test]
    fn link_records_are_set_used_revoked_carried_and_persisted() {
        const { assert!(core::LINK_RECORD_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN) };
        let a = SyncedState::new();
        let record = LinkRecord {
            hlc: 0,
            link_id: vec![1; 16],
            link_secret: vec![2; 12],
            kind: LinkRecordKind::Contact,
            ledger_id: vec![3; 6],
            revocation_seed: vec![4; 32],
            expiry_ms: 0,
            max_uses: 0,
            uses: 0,
            revoked: false,
            minted_at_ms: 9,
            label: "home".into(),
            answered: vec![],
            shared: false,
            editable: false,
            target_id: vec![],
            invite_seed: vec![],
            admission_version: 0,
            capability: vec![],
        };
        assert!(a.set_link_record(5, record.clone()).unwrap());
        assert!(a.all_link_records().unwrap()[0].editable);
        let bob = vec![0xB0; 32];
        assert_eq!(
            a.link_answer_verdict(vec![1; 16], vec![2; 12], bob.clone(), 0)
                .unwrap(),
            LinkAnswerVerdict::Accept
        );
        assert_eq!(
            a.link_answer_verdict(vec![1; 16], vec![2; 11], bob.clone(), 0)
                .unwrap(),
            LinkAnswerVerdict::Dead
        );
        assert!(a.record_link_use(6, vec![1; 16], bob.clone()).unwrap());
        assert_eq!(
            a.link_answer_verdict(vec![1; 16], vec![2; 12], bob, 0)
                .unwrap(),
            LinkAnswerVerdict::Answered
        );
        assert_eq!(
            a.link_answer_verdict(vec![1; 16], vec![2; 12], vec![0xB1; 32], 0)
                .unwrap(),
            LinkAnswerVerdict::Dead
        );
        let b = SyncedState::new();
        for e in a.link_record_envelopes().unwrap() {
            assert!(is_link_record_envelope(e.clone()));
            assert!(b.apply_link_record_envelope(e).unwrap());
        }
        assert_eq!(b.all_link_records().unwrap(), a.all_link_records().unwrap());
        assert!(b.revoke_link_record(7, vec![1; 16]).unwrap());
        let env = b.link_record_envelope_for(vec![1; 16]).unwrap().unwrap();
        assert!(a.apply_link_record_envelope(env).unwrap());
        let revoked = a.link_record(vec![1; 16]).unwrap().unwrap();
        assert!(revoked.revoked);
        // The seed that deletes the ledger row reached the other device and
        // survived its revocation.
        assert_eq!(revoked.revocation_seed, vec![4; 32]);
        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert_eq!(
            restored.all_link_records().unwrap(),
            a.all_link_records().unwrap()
        );
        assert!(matches!(
            a.set_link_record(
                1,
                LinkRecord {
                    link_id: vec![1; 15],
                    ..record
                }
            ),
            Err(FfiError::SyncedState(_))
        ));
        assert!(matches!(
            a.apply_link_record_envelope(b"lakko/sync/link-record/v1\0".to_vec()),
            Err(FfiError::SyncedState(_))
        ));
    }

    #[test]
    fn the_recovery_email_is_set_removed_carried_and_persisted() {
        let a = SyncedState::new();
        assert_eq!(a.recovery_email().unwrap(), None);
        assert_eq!(a.recovery_email_register().unwrap(), None);
        assert_eq!(a.recovery_email_envelope().unwrap(), None);

        // Normalised on the way in, with the one rule.
        assert!(a
            .set_recovery_email(5, Some("  Alice@Example.COM ".into()))
            .unwrap());
        assert_eq!(
            a.recovery_email().unwrap().as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(
            a.recovery_email().unwrap(),
            Some(
                crate::email_vault::normalize_recovery_email("  Alice@Example.COM ".into())
                    .unwrap()
            )
        );
        // The same write and an older one change nothing.
        assert!(!a
            .set_recovery_email(5, Some("alice@example.com".into()))
            .unwrap());
        assert!(!a
            .set_recovery_email(4, Some("old@example.com".into()))
            .unwrap());

        // Live send, and a re-send is not an update.
        let b = SyncedState::new();
        let live = a.recovery_email_envelope().unwrap().unwrap();
        assert!(is_recovery_email_envelope(live.clone()));
        assert!(!is_lineage_envelope(live.clone()));
        assert!(live.len() <= recovery_email_envelope_max_len() as usize);
        assert!(b.apply_recovery_email_envelope(live.clone()).unwrap());
        assert!(!b.apply_recovery_email_envelope(live.clone()).unwrap());
        assert_eq!(
            b.recovery_email_register().unwrap(),
            Some(RecoveryEmailRegister {
                hlc: 5,
                address: Some("alice@example.com".into()),
            })
        );

        // The snapshot parts do not carry it; persistence and merge do.
        assert_eq!(a.snapshot_parts().unwrap()[0].entry_count(), 0);
        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert_eq!(
            restored.recovery_email().unwrap().as_deref(),
            Some("alice@example.com")
        );
        let merged = SyncedState::new();
        assert!(merged.merge_from(restored).unwrap());
        assert_eq!(merged.to_bytes().unwrap(), a.to_bytes().unwrap());

        // A removal (None, and a blank string) is kept and beats the older
        // address everywhere it arrives; the stale envelope cannot undo it.
        assert!(a.set_recovery_email(7, None).unwrap());
        assert_eq!(a.recovery_email().unwrap(), None);
        assert_eq!(
            a.recovery_email_register().unwrap(),
            Some(RecoveryEmailRegister {
                hlc: 7,
                address: None,
            })
        );
        assert!(!a.apply_recovery_email_envelope(live).unwrap());
        let removal = a.recovery_email_envelope().unwrap().unwrap();
        assert!(b.apply_recovery_email_envelope(removal).unwrap());
        assert_eq!(b.recovery_email().unwrap(), None);
        let blank = SyncedState::new();
        assert!(blank.set_recovery_email(1, Some("   ".into())).unwrap());
        assert_eq!(
            blank.recovery_email_register().unwrap().unwrap().address,
            None
        );
        // A newer address beats the older removal.
        assert!(a
            .set_recovery_email(8, Some("new@example.com".into()))
            .unwrap());
        assert_eq!(
            a.recovery_email().unwrap().as_deref(),
            Some("new@example.com")
        );
        assert_eq!(a.max_hlc().unwrap(), 8);
    }

    #[test]
    fn recovery_email_refusals_carry_their_code_and_change_nothing() {
        let s = SyncedState::new();
        s.set_recovery_email(1, Some("a@b.example".into())).unwrap();
        for (raw, code) in [
            ("ab", "too-short: "),
            ("no-at-sign", "not-one-at-sign: "),
            ("a b@c", "whitespace: "),
            ("a@", "too-short: "),
            ("abc@", "empty-part: "),
        ] {
            match s.set_recovery_email(9, Some(raw.into())) {
                Err(FfiError::RecoveryEmail(m)) => assert!(m.starts_with(code), "{m}"),
                other => panic!("expected a refusal for {raw:?}, got {other:?}"),
            }
        }
        let mut bad = s.recovery_email_envelope().unwrap().unwrap();
        bad.push(0);
        match s.apply_recovery_email_envelope(bad) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("trailing: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        match s.apply_recovery_email_envelope(b"lakko/sync/lineage/v1\0".to_vec()) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("wrongDomain: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(!is_recovery_email_envelope(
            b"lakko/sync/lineage/v1\0".to_vec()
        ));
        assert_eq!(s.recovery_email().unwrap().as_deref(), Some("a@b.example"));
        assert_eq!(s.recovery_email_register().unwrap().unwrap().hlc, 1);
    }

    /// `lakko-sync` checks a stored address structurally and does not depend
    /// on `lakko-mnemonic`; this pins that the two agree: everything the one
    /// rule normalises is storable, and the length bounds are the same.
    #[test]
    fn the_sync_address_check_accepts_everything_the_one_rule_normalises() {
        use lakko_mnemonic::recovery_email as rule;
        const { assert!(core::RECOVERY_EMAIL_MIN_LEN == rule::RECOVERY_EMAIL_MIN_LEN) };
        const { assert!(core::RECOVERY_EMAIL_MAX_LEN == rule::RECOVERY_EMAIL_MAX_LEN) };
        const {
            assert!(core::RECOVERY_EMAIL_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);
        };
        assert!(
            recovery_email_envelope_max_len() <= crate::device_group_service_envelope_max_len()
        );
        let longest = format!("{}@B", "A".repeat(252));
        for raw in [
            "  A@B ",
            "Alice.Smith+Tag@Example.COM",
            "Юзер@Почта.рф",
            "ΟΔΟΣ@x.gr",
            "İ@x.tr",
            longest.as_str(),
        ] {
            let normalised = rule::normalize_recovery_email(raw).unwrap();
            assert_eq!(core::check_recovery_email_address(&normalised), Ok(()));
            let s = SyncedState::new();
            assert!(s.set_recovery_email(1, Some(raw.into())).unwrap());
            assert_eq!(s.recovery_email().unwrap(), Some(normalised));
        }
        // And what the one rule refuses, the structural check refuses too.
        for bad in ["ab", "abc", "a@b@c", "@ab", "ab@", "a b@c", "a@b\n"] {
            assert!(rule::normalize_recovery_email(bad).is_err() || bad.ends_with('\n'));
            assert!(core::check_recovery_email_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn device_acknowledgements_are_written_once_and_carried() {
        let a = SyncedState::new();
        let k = vec![7u8; 32];
        assert!(!a.is_device_acknowledged(k.clone()).unwrap());
        assert!(a.acknowledge_devices(1, vec![vec![1u8; 31]], 5).is_err());
        let sent = a.acknowledge_devices(3, vec![k.clone()], 5).unwrap();
        assert_eq!(sent.len(), 1);
        assert!(is_device_ack_envelope(sent[0].clone()));
        assert!(a
            .acknowledge_devices(4, vec![k.clone()], 6)
            .unwrap()
            .is_empty());
        let b = SyncedState::new();
        assert!(b.apply_device_ack_envelope(sent[0].clone()).unwrap());
        assert!(b.is_device_acknowledged(k.clone()).unwrap());
        assert!(!b.is_device_acknowledged(vec![7u8; 3]).unwrap());
        assert_eq!(b.acknowledged_devices().unwrap(), vec![k]);
        assert_eq!(b.device_ack_envelopes().unwrap(), sent);
        assert!(b.apply_device_ack_envelope(vec![1, 2, 3]).is_err());
    }

    #[test]
    fn the_own_username_is_set_cleared_carried_and_persisted() {
        let a = SyncedState::new();
        assert_eq!(a.own_username().unwrap(), None);
        assert_eq!(a.own_username_register().unwrap(), None);
        assert_eq!(a.own_username_envelope().unwrap(), None);

        // Normalised on the way in, with the one rule.
        assert!(a.set_own_username(5, Some(" @Bob_42 ".into())).unwrap());
        assert_eq!(a.own_username().unwrap().as_deref(), Some("bob_42"));
        assert_eq!(
            a.own_username().unwrap(),
            Some(crate::discovery::validate_username("@Bob_42".into()).unwrap())
        );
        assert!(!a.set_own_username(5, Some("bob_42".into())).unwrap());
        assert!(!a.set_own_username(4, Some("older".into())).unwrap());

        let b = SyncedState::new();
        let live = a.own_username_envelope().unwrap().unwrap();
        assert!(is_own_username_envelope(live.clone()));
        assert!(!is_recovery_email_envelope(live.clone()));
        assert!(!is_lineage_envelope(live.clone()));
        assert!(live.len() <= own_username_envelope_max_len() as usize);
        assert!(b.apply_own_username_envelope(live.clone()).unwrap());
        assert!(!b.apply_own_username_envelope(live.clone()).unwrap());
        assert_eq!(
            b.own_username_register().unwrap(),
            Some(OwnUsernameRegister {
                hlc: 5,
                username: Some("bob_42".into()),
            })
        );

        // The snapshot parts do not carry it; persistence and merge do.
        assert_eq!(a.snapshot_parts().unwrap()[0].entry_count(), 0);
        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.own_username().unwrap().as_deref(), Some("bob_42"));
        let merged = SyncedState::new();
        assert!(merged.merge_from(restored).unwrap());
        assert_eq!(merged.to_bytes().unwrap(), a.to_bytes().unwrap());

        // A rename wins over the old name everywhere; the stale envelope
        // cannot bring the old name back.
        assert!(a.set_own_username(6, Some("robert".into())).unwrap());
        assert!(!a.apply_own_username_envelope(live.clone()).unwrap());
        assert_eq!(a.own_username().unwrap().as_deref(), Some("robert"));

        // "None" (None, and a blank string) is kept and beats the older name.
        assert!(a.set_own_username(7, None).unwrap());
        assert_eq!(a.own_username().unwrap(), None);
        assert_eq!(
            a.own_username_register().unwrap(),
            Some(OwnUsernameRegister {
                hlc: 7,
                username: None,
            })
        );
        assert!(!a.apply_own_username_envelope(live).unwrap());
        let none = a.own_username_envelope().unwrap().unwrap();
        assert!(b.apply_own_username_envelope(none).unwrap());
        assert_eq!(b.own_username().unwrap(), None);
        let blank = SyncedState::new();
        assert!(blank.set_own_username(1, Some("   ".into())).unwrap());
        assert_eq!(
            blank.own_username_register().unwrap().unwrap().username,
            None
        );
        // A newer name beats the older "none".
        assert!(a.set_own_username(8, Some("bobby".into())).unwrap());
        assert_eq!(a.own_username().unwrap().as_deref(), Some("bobby"));
    }

    #[test]
    fn own_username_refusals_carry_their_reason_and_change_nothing() {
        const {
            assert!(core::OWN_USERNAME_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);
        };
        assert!(own_username_envelope_max_len() <= crate::device_group_service_envelope_max_len());
        let s = SyncedState::new();
        s.set_own_username(1, Some("bob".into())).unwrap();
        assert!(matches!(
            s.set_own_username(9, Some("ab".into())),
            Err(FfiError::UsernameTooShort(_))
        ));
        assert!(matches!(
            s.set_own_username(9, Some("a".repeat(33))),
            Err(FfiError::UsernameTooLong(_))
        ));
        assert!(matches!(
            s.set_own_username(9, Some("bo b".into())),
            Err(FfiError::UsernameInvalidCharacters(_))
        ));
        assert!(matches!(
            s.set_own_username(9, Some("@".into())),
            Err(FfiError::UsernameEmpty(_))
        ));
        let mut bad = s.own_username_envelope().unwrap().unwrap();
        bad.push(0);
        match s.apply_own_username_envelope(bad) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("trailing: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        let email = b"lakko/sync/recovery-email/v1\0".to_vec();
        match s.apply_own_username_envelope(email.clone()) {
            Err(FfiError::SyncedState(m)) => assert!(m.starts_with("wrongDomain: "), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(!is_own_username_envelope(email));
        assert_eq!(s.own_username().unwrap().as_deref(), Some("bob"));
        assert_eq!(s.own_username_register().unwrap().unwrap().hlc, 1);
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
                1_000 + message_id,
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
        assert_eq!(list[1].sent_at_ms, 1_007);
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

    fn with_end(register: ConversationEndRegister) -> FolderChangeSet {
        FolderChangeSet {
            conversation_ends: vec![register],
            ..FolderChangeSet::default()
        }
    }

    /// An end through the same path a client takes: compose, fold, a late
    /// device in the snapshot, a restore, a join clearing it.
    #[test]
    fn a_conversation_end_reaches_a_late_device_and_a_join_clears_it() {
        let a = SyncedState::new();
        // A first join leaves no register.
        assert_eq!(a.compose_conversation_joined(1, chat(), 3).unwrap(), None);

        let end = a
            .compose_conversation_ended(5, chat(), 7)
            .unwrap()
            .expect("the first end writes");
        assert_eq!(end.state, ConversationEndState::Ended);
        assert_eq!((end.hlc, end.epoch), (5, 7));
        assert!(a.apply_folder_changes(with_end(end.clone())).unwrap());
        assert!(a.is_conversation_ended(chat()).unwrap());
        assert_eq!(a.ended_conversations().unwrap(), vec![end.clone()]);
        assert_eq!(a.conversation_end(chat()).unwrap(), Some(end.clone()));
        // The same end from another device tells nobody anything.
        assert_eq!(a.compose_conversation_ended(9, chat(), 7).unwrap(), None);

        let late = SyncedState::new();
        for part in a.snapshot_parts().unwrap() {
            late.apply_payload(ParsedDeviceGroupPayload {
                hlc: 1,
                sender_leaf: 0,
                body: DeviceGroupBody::StateSnapshot { part },
            })
            .unwrap();
        }
        assert!(late.is_conversation_ended(chat()).unwrap());
        let restored = SyncedState::from_bytes(a.to_bytes().unwrap()).unwrap();
        assert!(restored.is_conversation_ended(chat()).unwrap());

        let join = a
            .compose_conversation_joined(6, chat(), 9)
            .unwrap()
            .expect("a join after an end writes");
        assert_eq!(join.state, ConversationEndState::Joined);
        assert!(a.apply_folder_changes(with_end(join)).unwrap());
        assert!(!a.is_conversation_ended(chat()).unwrap());
        assert_eq!(a.ended_conversations().unwrap(), vec![]);

        // The old end, sent late with the newest stamp, does not stand.
        let mut stale = end;
        stale.hlc = 99;
        assert!(!a.apply_folder_changes(with_end(stale)).unwrap());
        assert!(!a.is_conversation_ended(chat()).unwrap());
        assert_eq!(a.compose_conversation_ended(99, chat(), 7).unwrap(), None);

        let no_id = FolderConversationRef {
            kind: FolderConversationKind::Group,
            id: vec![],
        };
        assert!(matches!(
            a.compose_conversation_ended(1, no_id, 0),
            Err(FfiError::SyncedState(_))
        ));
        assert_eq!(
            conversation_ends_max_client() as usize,
            core::CONVERSATION_ENDS_MAX_CLIENT
        );
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
                            sent_at_ms: 90,
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
                90,
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
            0,
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
            0,
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
            0,
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
            0,
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
            0,
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
            sent_at_ms: 0,
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

    #[test]
    fn quick_reaction_and_own_channel_activity_sync_through_envelopes() {
        let a = SyncedState::new();
        let b = SyncedState::new();
        assert!(a.set_quick_reaction(1, "\u{1F44D}".into()).unwrap());
        let env = a.quick_reaction_envelope().unwrap().unwrap();
        assert!(is_quick_reaction_envelope(env.clone()));
        assert!(b.apply_quick_reaction_envelope(env).unwrap());
        assert_eq!(b.quick_reaction().unwrap(), Some("\u{1F44D}".to_string()));

        let channel = vec![3_u8; 16];
        let env = a
            .set_channel_reaction(2, channel.clone(), 7, "\u{1F44D}".into(), true, 1_000)
            .unwrap()
            .unwrap();
        assert!(is_channel_reaction_envelope(env.clone()));
        assert!(b.apply_channel_reaction_envelope(env).unwrap());
        assert!(b
            .has_channel_reaction(channel.clone(), 7, "\u{1F44D}".into())
            .unwrap());
        assert_eq!(a.channel_reaction_envelopes().unwrap().len(), 1);
        // A reaction on a comment is its own register.
        let env = a
            .set_comment_reaction(5, channel.clone(), 7, 12, "\u{1F44D}".into(), true, 1_000)
            .unwrap()
            .unwrap();
        assert!(b.apply_channel_reaction_envelope(env).unwrap());
        assert!(b
            .has_comment_reaction(channel.clone(), 7, 12, "\u{1F44D}".into())
            .unwrap());
        assert_eq!(
            b.comment_reactions_on(channel.clone(), 7, 12).unwrap(),
            vec!["\u{1F44D}".to_string()]
        );
        assert_eq!(
            b.channel_reactions_on(channel.clone(), 7).unwrap(),
            vec!["\u{1F44D}".to_string()]
        );

        let env = a
            .record_channel_comment(3, channel.clone(), 7, vec![9; 16], 1_000)
            .unwrap()
            .unwrap();
        assert!(is_channel_comment_envelope(env.clone()));
        assert!(b.apply_channel_comment_envelope(env).unwrap());
        assert_eq!(
            b.channel_comments_on(channel.clone(), 7).unwrap(),
            vec![vec![9; 16]]
        );
        assert!(a
            .delete_channel_comment(4, channel.clone(), 7, vec![9; 16], 1_000)
            .unwrap()
            .is_some());
        assert!(a.channel_comments_on(channel, 7).unwrap().is_empty());
        assert_eq!(a.prune_channel_activity(u64::MAX).unwrap(), 3);
        assert_eq!(channel_reactions_per_post_max(), 3);
    }
}
