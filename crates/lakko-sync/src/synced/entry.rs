// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The registers the account's devices keep in step (ADR-0058), and the
//! saved-message register ADR-0065 section 4 adds to them.
//!
//! Plain Rust types, owned here so the fold, its tests and the persistence
//! codec need no protobuf. `lakko-mls` converts the DeviceGroup proto into
//! these types at decode time and runs [`FolderChanges::validate`] /
//! [`StateSnapshot::validate`] there, so the bounds are written once and a
//! payload that breaks one is refused whole.
//!
//! Every entry has a **canonical encoding** ([`SyncEntry::canonical_bytes`]).
//! It is what breaks an exact stamp tie in the fold (larger bytes win) and
//! what the persistence codec stores. It is Lakko's own length-prefixed
//! layout, not the protobuf encoding: the fold runs in Rust on every client,
//! so one deterministic layout is all a tie-break needs, and owning it keeps
//! this crate free of a protobuf dependency.

use thiserror::Error;

/// Length of a folder id: 16 random bytes minted once.
pub const FOLDER_ID_LEN: usize = 16;
/// A folder name's maximum length in Unicode scalar values.
pub const FOLDER_NAME_MAX_CHARS: usize = 24;
/// A folder name's maximum length in UTF-8 bytes.
pub const FOLDER_NAME_MAX_BYTES: usize = 96;
/// A conversation reference id's maximum length in bytes.
pub const CONVERSATION_REF_ID_MAX_LEN: usize = 64;
/// The most pins one folder's pin list may carry on the wire.
pub const FOLDER_PINS_MAX: usize = 10;
/// The most folder ids a folder order may carry on the wire.
pub const FOLDER_ORDER_MAX: usize = 256;
/// The most registers one [`FolderChanges`] may carry.
pub const FOLDER_CHANGES_MAX_REGISTERS: usize = 1024;
/// The most parts a [`StateSnapshot`] may be split into.
pub const SNAPSHOT_MAX_PARTS: u32 = 64;

/// Length of a blob id: the SHA-256 of the stored ciphertext.
pub const SAVED_BLOB_ID_LEN: usize = 32;
/// The most blob ids (files and thumbnails) one saved message may carry.
pub const SAVED_BLOB_IDS_MAX: usize = 128;
/// A saved message's author snapshot: maximum length in UTF-8 bytes.
pub const SAVED_AUTHOR_MAX_BYTES: usize = 255;
/// A saved message's excerpt snapshot: maximum length in UTF-8 bytes.
pub const SAVED_EXCERPT_MAX_BYTES: usize = 512;
/// A saved message's attachment kind: maximum length in UTF-8 bytes.
pub const SAVED_ATTACHMENT_KIND_MAX_BYTES: usize = 32;
/// How many saved messages a client lets one account hold (ADR-0065
/// section 4).
///
/// A **client-side creation limit**, chosen, not measured: the client
/// refuses a new Save once [`super::SyncedState::saved`] holds this many.
/// It is **not a decode bound**. A state, a change set or a snapshot
/// carrying more saved registers than this decodes and folds like any
/// other, because two devices that each saved while offline can
/// legitimately exceed it together.
pub const SAVED_MAX_CLIENT: usize = 2000;
/// How many saved registers a client puts in one live change set.
///
/// A **client-side sending rule**, chosen, and **not a decode bound**: a
/// live change set is one DeviceGroup message and is never split, and a
/// saved register at its bounds is about 5 KiB on the wire (128 blob ids
/// of 32 bytes are most of it), so the 1024-register cap alone would let
/// one message outgrow what the Delivery Service accepts. At most this
/// many saved registers always encode below
/// [`super::SNAPSHOT_MAX_PART_BYTES`] (pinned by a test in `lakko-mls`,
/// which owns the wire encoding); a larger selection goes out as several
/// change sets.
pub const SAVED_MAX_PER_CHANGE_SET: usize = 32;

/// `SavedMessage` state on the wire and in the canonical encoding: saved.
pub const SAVED_STATE_PRESENT: u8 = 1;
/// `SavedMessage` state on the wire and in the canonical encoding: removed
/// (the tombstone). Greater than [`SAVED_STATE_PRESENT`] on purpose: on an
/// exact stamp tie between a save and a remove of one message the larger
/// canonical bytes win, so the remove does, on every device.
pub const SAVED_STATE_REMOVED: u8 = 2;

/// `include_kinds` bit: one-to-one conversations.
pub const INCLUDE_ONE_TO_ONE: u32 = 1 << 0;
/// `include_kinds` bit: groups.
pub const INCLUDE_GROUPS: u32 = 1 << 1;
/// `include_kinds` bit: channels.
pub const INCLUDE_CHANNELS: u32 = 1 << 2;
/// `exclude_flags` bit: muted conversations.
pub const EXCLUDE_MUTED: u32 = 1 << 0;
/// `exclude_flags` bit: conversations with nothing unread.
pub const EXCLUDE_READ: u32 = 1 << 1;
/// `exclude_flags` bit: archived conversations.
pub const EXCLUDE_ARCHIVED: u32 = 1 << 2;
/// Every bit a v1 bitfield may carry. A set bit outside this mask is a
/// toggle from a newer client this build cannot honour, and is refused
/// rather than read as "off".
pub const KNOWN_BITS: u32 = 0b111;

/// A folder id.
pub type FolderId = [u8; FOLDER_ID_LEN];

/// A blob id: what `services/storage` addresses a ciphertext blob by.
pub type BlobId = [u8; SAVED_BLOB_ID_LEN];

/// Why an entry, a change set or a snapshot was refused (ADR-0058 section 4,
/// ADR-0065 section 4).
///
/// A reason, never the offending bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BoundsError {
    /// A folder id was not exactly 16 bytes.
    #[error("folder id is not {FOLDER_ID_LEN} bytes")]
    FolderIdLength,
    /// A folder name was empty.
    #[error("folder name is empty")]
    NameEmpty,
    /// A folder name was longer than 24 scalar values or 96 bytes.
    #[error("folder name is too long")]
    NameTooLong,
    /// A folder name failed the display-name character rule.
    #[error("folder name fails the display-name rule")]
    NameCharacters,
    /// A conversation reference named no kind.
    #[error("conversation reference kind is unspecified")]
    RefKindUnspecified,
    /// A conversation reference id was empty or longer than 64 bytes.
    #[error("conversation reference id length is out of bounds")]
    RefIdLength,
    /// A bit outside the three known ones was set in a bitfield.
    #[error("unknown bit set in a folder bitfield")]
    UnknownBits,
    /// A member register named a state this build does not know.
    #[error("unknown folder member state")]
    UnknownMemberState,
    /// A pin list was longer than 10.
    #[error("folder pin list is longer than {FOLDER_PINS_MAX}")]
    TooManyPins,
    /// A folder order was longer than 256.
    #[error("folder order is longer than {FOLDER_ORDER_MAX}")]
    OrderTooLong,
    /// A change set carried more than 1024 registers.
    #[error("folder change set carries more than {FOLDER_CHANGES_MAX_REGISTERS} registers")]
    TooManyRegisters,
    /// A snapshot part index was not below its part count, or the count was
    /// zero or above 64.
    #[error("snapshot part index or count is out of bounds")]
    SnapshotPart,
    /// A required sub-message was absent.
    #[error("a required field is missing")]
    Missing,
    /// A saved message's author snapshot was longer than 255 bytes.
    #[error("saved message author is longer than {SAVED_AUTHOR_MAX_BYTES} bytes")]
    SavedAuthorTooLong,
    /// A saved message's excerpt snapshot was longer than 512 bytes.
    #[error("saved message excerpt is longer than {SAVED_EXCERPT_MAX_BYTES} bytes")]
    SavedExcerptTooLong,
    /// A saved message's attachment kind was longer than 32 bytes.
    #[error(
        "saved message attachment kind is longer than {SAVED_ATTACHMENT_KIND_MAX_BYTES} bytes"
    )]
    SavedAttachmentKindTooLong,
    /// A saved message carried more than 128 blob ids.
    #[error("saved message carries more than {SAVED_BLOB_IDS_MAX} blob ids")]
    TooManyBlobIds,
    /// A blob id was not exactly 32 bytes.
    #[error("blob id is not {SAVED_BLOB_ID_LEN} bytes")]
    BlobIdLength,
    /// A saved message named one blob id twice.
    #[error("saved message repeats a blob id")]
    DuplicateBlobId,
    /// A saved-message register named a state this build does not know
    /// (including the unspecified `0`).
    #[error("unknown saved message state")]
    UnknownSavedState,
    /// A removed saved-message register carried a save time, a snapshot or
    /// blob ids. A tombstone has none.
    #[error("removed saved message carries a value")]
    SavedRemovedCarriesValue,
}

/// Which kind of conversation a [`ConversationRef`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConversationRefKind {
    /// A one-to-one conversation, by MLS group id.
    OneToOne,
    /// A group, by MLS group id.
    Group,
    /// A channel, by channel id.
    Channel,
}

impl ConversationRefKind {
    /// The wire number (the proto enum value).
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::OneToOne => 1,
            Self::Group => 2,
            Self::Channel => 3,
        }
    }

    /// Read a wire number; `0` (unspecified) and anything unknown refused.
    ///
    /// # Errors
    ///
    /// [`BoundsError::RefKindUnspecified`].
    pub const fn from_wire(v: i64) -> Result<Self, BoundsError> {
        match v {
            1 => Ok(Self::OneToOne),
            2 => Ok(Self::Group),
            3 => Ok(Self::Channel),
            _ => Err(BoundsError::RefKindUnspecified),
        }
    }
}

/// A conversation a folder names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConversationRef {
    /// Which kind it is.
    pub kind: ConversationRefKind,
    /// The MLS group id, or the channel id. 1..=64 bytes.
    pub id: Vec<u8>,
}

impl ConversationRef {
    /// Check the id length.
    ///
    /// # Errors
    ///
    /// [`BoundsError::RefIdLength`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.id.is_empty() || self.id.len() > CONVERSATION_REF_ID_MAX_LEN {
            return Err(BoundsError::RefIdLength);
        }
        Ok(())
    }
}

/// A conversation's membership in one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberState {
    /// Neither added nor excluded by hand: the element tombstone.
    None,
    /// Added by hand.
    Included,
    /// Excluded by hand.
    Excluded,
}

impl MemberState {
    /// The wire number.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Included => 1,
            Self::Excluded => 2,
        }
    }

    /// Read a wire number.
    ///
    /// # Errors
    ///
    /// [`BoundsError::UnknownMemberState`].
    pub const fn from_wire(v: i64) -> Result<Self, BoundsError> {
        match v {
            0 => Ok(Self::None),
            1 => Ok(Self::Included),
            2 => Ok(Self::Excluded),
            _ => Err(BoundsError::UnknownMemberState),
        }
    }
}

/// Turn a wire folder id into a [`FolderId`].
///
/// # Errors
///
/// [`BoundsError::FolderIdLength`].
pub fn folder_id_from_slice(bytes: &[u8]) -> Result<FolderId, BoundsError> {
    <FolderId>::try_from(bytes).map_err(|_| BoundsError::FolderIdLength)
}

/// Turn a wire blob id into a [`BlobId`].
///
/// # Errors
///
/// [`BoundsError::BlobIdLength`].
pub fn blob_id_from_slice(bytes: &[u8]) -> Result<BlobId, BoundsError> {
    <BlobId>::try_from(bytes).map_err(|_| BoundsError::BlobIdLength)
}

/// A folder's name and rule toggles (the meta register).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMeta {
    /// The folder.
    pub folder_id: FolderId,
    /// The name.
    pub name: String,
    /// `INCLUDE_*` bits.
    pub include_kinds: u32,
    /// `EXCLUDE_*` bits.
    pub exclude_flags: u32,
}

/// Check a folder name against ADR-0058 section 4.
///
/// # Errors
///
/// [`BoundsError::NameEmpty`], [`BoundsError::NameTooLong`] or
/// [`BoundsError::NameCharacters`].
pub fn check_folder_name(name: &str) -> Result<(), BoundsError> {
    if name.is_empty() {
        return Err(BoundsError::NameEmpty);
    }
    if name.len() > FOLDER_NAME_MAX_BYTES || name.chars().count() > FOLDER_NAME_MAX_CHARS {
        return Err(BoundsError::NameTooLong);
    }
    lakko_contacts::profile::check_display_name(name).map_err(|_| BoundsError::NameCharacters)
}

impl FolderMeta {
    /// Check the name and both bitfields.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        check_folder_name(&self.name)?;
        if self.include_kinds & !KNOWN_BITS != 0 || self.exclude_flags & !KNOWN_BITS != 0 {
            return Err(BoundsError::UnknownBits);
        }
        Ok(())
    }
}

/// One conversation's membership register in one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMember {
    /// The folder.
    pub folder_id: FolderId,
    /// The conversation.
    pub conversation: ConversationRef,
    /// Its state.
    pub state: MemberState,
}

/// A folder's ordered pin list, as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPins {
    /// The folder.
    pub folder_id: FolderId,
    /// Pinned conversations, top first.
    pub pinned: Vec<ConversationRef>,
}

impl FolderPins {
    /// Check the pin count and every reference.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.pinned.len() > FOLDER_PINS_MAX {
            return Err(BoundsError::TooManyPins);
        }
        self.pinned.iter().try_for_each(ConversationRef::validate)
    }
}

/// The account's folder order, as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderOrder {
    /// Folder ids, first tab first.
    pub folder_ids: Vec<FolderId>,
}

/// Per-conversation flags (the existing `ThreadFlags` payload).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct FlagsEntry {
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

/// A read cursor (the existing `ReadCursor` payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadCursorEntry {
    /// The conversation's MLS group id.
    pub group_id: Vec<u8>,
    /// Read up to this message id.
    pub up_to_message_id: Vec<u8>,
    /// Wall-clock millis of the read. Display only.
    pub read_at_ms: u64,
}

/// A draft (the existing `DraftUpdate` payload). Empty `text` = cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftEntry {
    /// The conversation's MLS group id.
    pub group_id: Vec<u8>,
    /// Composer text.
    pub text: String,
    /// Replying to this message id, or empty.
    pub reply_to_id: Vec<u8>,
}

/// The explicit conversation order (the existing `ConversationOrder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationOrderEntry {
    /// Group ids, top first.
    pub group_ids: Vec<Vec<u8>>,
}

/// What a saved-message row shows before the device holds the message
/// itself (ADR-0065 section 4): taken once, when the message is saved, and
/// never re-read from the original.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedSnapshot {
    /// The author's display name as the saving device showed it. At most
    /// 255 bytes.
    pub author: String,
    /// The start of the message text. At most 512 bytes.
    pub excerpt: String,
    /// A short token naming the kind of the message's attachment, which the
    /// client maps to its own label. Empty when the message has no
    /// attachment. At most 32 bytes.
    pub attachment_kind: String,
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a
/// character boundary.
fn fit(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

impl SavedSnapshot {
    /// Build a snapshot from what the client holds, cutting `author` and
    /// `excerpt` to their byte bounds on a character boundary. An excerpt is
    /// a prefix by definition, so a long message is cut, not refused.
    /// `attachment_kind` is a token and is never cut: an over-long one is
    /// refused by [`Self::validate`].
    #[must_use]
    pub fn fitted(author: &str, excerpt: &str, attachment_kind: &str) -> Self {
        Self {
            author: fit(author, SAVED_AUTHOR_MAX_BYTES).to_owned(),
            excerpt: fit(excerpt, SAVED_EXCERPT_MAX_BYTES).to_owned(),
            attachment_kind: attachment_kind.to_owned(),
        }
    }

    /// Check the three byte bounds.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.author.len() > SAVED_AUTHOR_MAX_BYTES {
            return Err(BoundsError::SavedAuthorTooLong);
        }
        if self.excerpt.len() > SAVED_EXCERPT_MAX_BYTES {
            return Err(BoundsError::SavedExcerptTooLong);
        }
        if self.attachment_kind.len() > SAVED_ATTACHMENT_KIND_MAX_BYTES {
            return Err(BoundsError::SavedAttachmentKindTooLong);
        }
        Ok(())
    }
}

/// The value of one saved-message register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedValue {
    /// The message is saved.
    Present {
        /// Wall-clock millis of the save. Orders the saved list; never
        /// decides which write wins (the stamp does).
        saved_at_ms: u64,
        /// What a row shows before the message itself is held.
        snapshot: SavedSnapshot,
        /// The blob ids of the message's files and thumbnails, so any device
        /// of the account can keep them alive (ADR-0065 section 3). At most
        /// 128, each named once.
        blob_ids: Vec<BlobId>,
    },
    /// The message was removed from saved: the tombstone. Kept forever, so a
    /// device that was offline across the removal cannot bring the save
    /// back; a later save with a greater stamp saves the message again.
    Removed,
}

impl SavedValue {
    /// The state number on the wire and in the canonical encoding.
    #[must_use]
    pub const fn state_wire(&self) -> u8 {
        match self {
            Self::Present { .. } => SAVED_STATE_PRESENT,
            Self::Removed => SAVED_STATE_REMOVED,
        }
    }
}

/// One saved-message register (ADR-0065 section 4), keyed by the
/// conversation and the message's Delivery Service journal id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMessage {
    /// The conversation the message is in.
    pub conversation: ConversationRef,
    /// The message's Delivery Service journal id.
    pub message_id: u64,
    /// Saved, or removed.
    pub value: SavedValue,
}

impl SavedMessage {
    /// Compose a save from what the client holds: `snapshot` is cut with
    /// [`SavedSnapshot::fitted`] (so a caller may pass the whole message
    /// text), a blob id named twice is kept once (first occurrence), and
    /// the result is validated.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies: a blob id that is not 32
    /// bytes, more than 128 distinct blob ids, an over-long attachment
    /// kind, or a bad conversation reference.
    pub fn present<B: AsRef<[u8]>>(
        conversation: ConversationRef,
        message_id: u64,
        saved_at_ms: u64,
        snapshot: &SavedSnapshot,
        blob_ids: &[B],
    ) -> Result<Self, BoundsError> {
        let mut seen = std::collections::BTreeSet::new();
        let mut distinct = Vec::new();
        for b in blob_ids {
            let id = blob_id_from_slice(b.as_ref())?;
            if seen.insert(id) {
                if distinct.len() == SAVED_BLOB_IDS_MAX {
                    return Err(BoundsError::TooManyBlobIds);
                }
                distinct.push(id);
            }
        }
        let saved = Self {
            conversation,
            message_id,
            value: SavedValue::Present {
                saved_at_ms,
                snapshot: SavedSnapshot::fitted(
                    &snapshot.author,
                    &snapshot.excerpt,
                    &snapshot.attachment_kind,
                ),
                blob_ids: distinct,
            },
        };
        saved.validate()?;
        Ok(saved)
    }

    /// Compose a removal (the tombstone).
    ///
    /// # Errors
    ///
    /// [`BoundsError::RefIdLength`].
    pub fn removed(conversation: ConversationRef, message_id: u64) -> Result<Self, BoundsError> {
        conversation.validate()?;
        Ok(Self {
            conversation,
            message_id,
            value: SavedValue::Removed,
        })
    }

    /// Check the conversation reference and, for a save, the snapshot
    /// bounds, the blob-id count and that no blob id repeats. The count is
    /// checked first, so an oversized list costs no further work.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        self.conversation.validate()?;
        match &self.value {
            SavedValue::Removed => Ok(()),
            SavedValue::Present {
                snapshot, blob_ids, ..
            } => {
                if blob_ids.len() > SAVED_BLOB_IDS_MAX {
                    return Err(BoundsError::TooManyBlobIds);
                }
                snapshot.validate()?;
                let mut sorted: Vec<&BlobId> = blob_ids.iter().collect();
                sorted.sort_unstable();
                if sorted.windows(2).any(|w| w[0] == w[1]) {
                    return Err(BoundsError::DuplicateBlobId);
                }
                Ok(())
            },
        }
    }
}

/// One register value of any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEntry {
    /// A folder's meta register.
    FolderMeta(FolderMeta),
    /// A folder member register.
    FolderMember(FolderMember),
    /// A folder's pin list.
    FolderPins(FolderPins),
    /// A folder deletion (tombstone).
    FolderRemoved(FolderId),
    /// The folder order.
    FolderOrder(FolderOrder),
    /// A conversation's flags.
    Flags(FlagsEntry),
    /// A conversation's read cursor.
    ReadCursor(ReadCursorEntry),
    /// A conversation's draft.
    Draft(DraftEntry),
    /// The explicit conversation order.
    ConversationOrder(ConversationOrderEntry),
    /// A saved message, or its removal (ADR-0065 section 4).
    Saved(SavedMessage),
}

/// Which register an entry writes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegisterKey {
    /// `FolderMeta` of this folder.
    FolderMeta(FolderId),
    /// `FolderMember` of this folder and conversation.
    FolderMember(FolderId, ConversationRef),
    /// `FolderPins` of this folder.
    FolderPins(FolderId),
    /// The tombstone of this folder.
    FolderRemoved(FolderId),
    /// The folder order.
    FolderOrder,
    /// Flags of this group.
    Flags(Vec<u8>),
    /// Read cursor of this group.
    ReadCursor(Vec<u8>),
    /// Draft of this group.
    Draft(Vec<u8>),
    /// The explicit conversation order.
    ConversationOrder,
    /// The saved register of this message in this conversation.
    Saved(ConversationRef, u64),
}

impl RegisterKey {
    /// The folder a folder-scoped register belongs to (not the tombstone,
    /// not the order).
    #[must_use]
    pub const fn folder_scoped(&self) -> Option<&FolderId> {
        match self {
            Self::FolderMeta(f) | Self::FolderMember(f, _) | Self::FolderPins(f) => Some(f),
            _ => None,
        }
    }
}

// Canonical-encoding tags. Frozen: they order equal-stamp ties and are what
// a persisted state is read back from.
const TAG_META: u8 = 1;
const TAG_MEMBER: u8 = 2;
const TAG_PINS: u8 = 3;
const TAG_REMOVED: u8 = 4;
const TAG_FOLDER_ORDER: u8 = 5;
const TAG_FLAGS: u8 = 6;
const TAG_CURSOR: u8 = 7;
const TAG_DRAFT: u8 = 8;
const TAG_CONV_ORDER: u8 = 9;
const TAG_SAVED: u8 = 10;

impl SyncEntry {
    /// The register this entry writes.
    #[must_use]
    pub fn key(&self) -> RegisterKey {
        match self {
            Self::FolderMeta(m) => RegisterKey::FolderMeta(m.folder_id),
            Self::FolderMember(m) => RegisterKey::FolderMember(m.folder_id, m.conversation.clone()),
            Self::FolderPins(p) => RegisterKey::FolderPins(p.folder_id),
            Self::FolderRemoved(f) => RegisterKey::FolderRemoved(*f),
            Self::FolderOrder(_) => RegisterKey::FolderOrder,
            Self::Flags(f) => RegisterKey::Flags(f.group_id.clone()),
            Self::ReadCursor(c) => RegisterKey::ReadCursor(c.group_id.clone()),
            Self::Draft(d) => RegisterKey::Draft(d.group_id.clone()),
            Self::ConversationOrder(_) => RegisterKey::ConversationOrder,
            Self::Saved(m) => RegisterKey::Saved(m.conversation.clone(), m.message_id),
        }
    }

    /// Check the ADR-0058 section 4 and ADR-0065 section 4 bounds that
    /// apply to this one entry. The four older kinds (flags, read cursor,
    /// draft, conversation order) carry no bounds of their own.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        match self {
            Self::FolderMeta(m) => m.validate(),
            Self::FolderMember(m) => m.conversation.validate(),
            Self::FolderPins(p) => p.validate(),
            Self::FolderOrder(o) => {
                if o.folder_ids.len() > FOLDER_ORDER_MAX {
                    Err(BoundsError::OrderTooLong)
                } else {
                    Ok(())
                }
            },
            Self::Saved(m) => m.validate(),
            Self::FolderRemoved(_)
            | Self::Flags(_)
            | Self::ReadCursor(_)
            | Self::Draft(_)
            | Self::ConversationOrder(_) => Ok(()),
        }
    }

    /// The canonical encoding: a tag byte then the fields, every
    /// variable-length field prefixed by its length (u8 for a conversation
    /// reference id, u32 big-endian otherwise; counts u32 big-endian).
    ///
    /// Deterministic and injective per register, so on an exact stamp tie
    /// "larger bytes win" picks the same value on every device.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::FolderMeta(m) => {
                out.push(TAG_META);
                out.extend_from_slice(&m.folder_id);
                put_bytes32(&mut out, m.name.as_bytes());
                out.extend_from_slice(&m.include_kinds.to_be_bytes());
                out.extend_from_slice(&m.exclude_flags.to_be_bytes());
            },
            Self::FolderMember(m) => {
                out.push(TAG_MEMBER);
                out.extend_from_slice(&m.folder_id);
                put_ref(&mut out, &m.conversation);
                out.push(m.state.to_wire());
            },
            Self::FolderPins(p) => {
                out.push(TAG_PINS);
                out.extend_from_slice(&p.folder_id);
                put_len(&mut out, p.pinned.len());
                for r in &p.pinned {
                    put_ref(&mut out, r);
                }
            },
            Self::FolderRemoved(f) => {
                out.push(TAG_REMOVED);
                out.extend_from_slice(f);
            },
            Self::FolderOrder(o) => {
                out.push(TAG_FOLDER_ORDER);
                put_len(&mut out, o.folder_ids.len());
                for f in &o.folder_ids {
                    out.extend_from_slice(f);
                }
            },
            Self::Flags(f) => {
                out.push(TAG_FLAGS);
                put_bytes32(&mut out, &f.group_id);
                out.push(u8::from(f.pinned));
                out.push(u8::from(f.muted));
                out.push(u8::from(f.archived));
                out.push(u8::from(f.unread_override));
            },
            Self::ReadCursor(c) => {
                out.push(TAG_CURSOR);
                put_bytes32(&mut out, &c.group_id);
                put_bytes32(&mut out, &c.up_to_message_id);
                out.extend_from_slice(&c.read_at_ms.to_be_bytes());
            },
            Self::Draft(d) => {
                out.push(TAG_DRAFT);
                put_bytes32(&mut out, &d.group_id);
                put_bytes32(&mut out, d.text.as_bytes());
                put_bytes32(&mut out, &d.reply_to_id);
            },
            Self::ConversationOrder(o) => {
                out.push(TAG_CONV_ORDER);
                put_len(&mut out, o.group_ids.len());
                for g in &o.group_ids {
                    put_bytes32(&mut out, g);
                }
            },
            // The key (reference, message id) comes first and the state
            // byte directly after it, so on an exact stamp tie the two
            // writes of one register differ first at the state byte and a
            // removal beats a save.
            Self::Saved(m) => {
                out.push(TAG_SAVED);
                put_ref(&mut out, &m.conversation);
                out.extend_from_slice(&m.message_id.to_be_bytes());
                out.push(m.value.state_wire());
                if let SavedValue::Present {
                    saved_at_ms,
                    snapshot,
                    blob_ids,
                } = &m.value
                {
                    out.extend_from_slice(&saved_at_ms.to_be_bytes());
                    put_bytes32(&mut out, snapshot.author.as_bytes());
                    put_bytes32(&mut out, snapshot.excerpt.as_bytes());
                    put_bytes32(&mut out, snapshot.attachment_kind.as_bytes());
                    put_len(&mut out, blob_ids.len());
                    for b in blob_ids {
                        out.extend_from_slice(b);
                    }
                }
            },
        }
        out
    }

    /// Read one canonical encoding back, then [`Self::validate`] it.
    ///
    /// # Errors
    ///
    /// [`CodecError`] on a malformed or out-of-bounds entry, or trailing
    /// bytes.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut cur = bytes;
        let entry = match take_u8(&mut cur)? {
            TAG_META => {
                let folder_id = take_fid(&mut cur)?;
                let name = take_string(&mut cur)?;
                let include_kinds = take_u32(&mut cur)?;
                let exclude_flags = take_u32(&mut cur)?;
                Self::FolderMeta(FolderMeta {
                    folder_id,
                    name,
                    include_kinds,
                    exclude_flags,
                })
            },
            TAG_MEMBER => {
                let folder_id = take_fid(&mut cur)?;
                let conversation = take_ref(&mut cur)?;
                let state = MemberState::from_wire(i64::from(take_u8(&mut cur)?))?;
                Self::FolderMember(FolderMember {
                    folder_id,
                    conversation,
                    state,
                })
            },
            TAG_PINS => {
                let folder_id = take_fid(&mut cur)?;
                let n = take_u32(&mut cur)? as usize;
                if n > FOLDER_PINS_MAX {
                    return Err(BoundsError::TooManyPins.into());
                }
                let mut pinned = Vec::with_capacity(n);
                for _ in 0..n {
                    pinned.push(take_ref(&mut cur)?);
                }
                Self::FolderPins(FolderPins { folder_id, pinned })
            },
            TAG_REMOVED => Self::FolderRemoved(take_fid(&mut cur)?),
            TAG_FOLDER_ORDER => {
                let n = take_u32(&mut cur)? as usize;
                if n > FOLDER_ORDER_MAX {
                    return Err(BoundsError::OrderTooLong.into());
                }
                let mut folder_ids = Vec::with_capacity(n);
                for _ in 0..n {
                    folder_ids.push(take_fid(&mut cur)?);
                }
                Self::FolderOrder(FolderOrder { folder_ids })
            },
            TAG_FLAGS => {
                let group_id = take_bytes32(&mut cur)?;
                let pinned = take_bool(&mut cur)?;
                let muted = take_bool(&mut cur)?;
                let archived = take_bool(&mut cur)?;
                let unread_override = take_bool(&mut cur)?;
                Self::Flags(FlagsEntry {
                    group_id,
                    pinned,
                    muted,
                    archived,
                    unread_override,
                })
            },
            TAG_CURSOR => {
                let group_id = take_bytes32(&mut cur)?;
                let up_to_message_id = take_bytes32(&mut cur)?;
                let read_at_ms = take_u64(&mut cur)?;
                Self::ReadCursor(ReadCursorEntry {
                    group_id,
                    up_to_message_id,
                    read_at_ms,
                })
            },
            TAG_DRAFT => {
                let group_id = take_bytes32(&mut cur)?;
                let text = take_string(&mut cur)?;
                let reply_to_id = take_bytes32(&mut cur)?;
                Self::Draft(DraftEntry {
                    group_id,
                    text,
                    reply_to_id,
                })
            },
            TAG_CONV_ORDER => {
                let n = take_u32(&mut cur)? as usize;
                // Grown rather than pre-sized: the count is read off the
                // bytes and a malformed state must not ask for a large
                // allocation.
                let mut group_ids = Vec::new();
                for _ in 0..n {
                    group_ids.push(take_bytes32(&mut cur)?);
                }
                Self::ConversationOrder(ConversationOrderEntry { group_ids })
            },
            TAG_SAVED => Self::Saved(take_saved(&mut cur)?),
            _ => return Err(CodecError::UnknownTag),
        };
        if !cur.is_empty() {
            return Err(CodecError::Trailing);
        }
        entry.validate()?;
        Ok(entry)
    }
}

/// Why persisted bytes were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodecError {
    /// The bytes ended early.
    #[error("synced state bytes are truncated")]
    Truncated,
    /// An entry tag this build does not know.
    #[error("unknown synced entry tag")]
    UnknownTag,
    /// The version byte is not the one this build writes.
    #[error("unknown synced state version")]
    UnknownVersion,
    /// A text field is not UTF-8.
    #[error("synced state text is not UTF-8")]
    NotUtf8,
    /// A boolean byte was neither 0 nor 1.
    #[error("synced state boolean is not 0 or 1")]
    BadBool,
    /// Bytes remained after an entry or after the state.
    #[error("trailing bytes in synced state")]
    Trailing,
    /// A length did not fit its field.
    #[error("synced state field is too long to encode")]
    TooLong,
    /// An entry broke an ADR-0058 or ADR-0065 bound.
    #[error("synced entry out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

/// A live edit: only the registers that changed, each with its own stamp.
///
/// Named for the chat folders it was built to carry (ADR-0058); since
/// ADR-0065 section 4 it also carries saved-message registers, so one Save
/// or Remove travels exactly like a folder edit and a snapshot's
/// [`StateSnapshot::folders`] hands a late device the saved list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderChanges {
    /// Meta registers.
    pub metas: Vec<Stamped<FolderMeta>>,
    /// Member registers.
    pub members: Vec<Stamped<FolderMember>>,
    /// Pin-list registers.
    pub pins: Vec<Stamped<FolderPins>>,
    /// Tombstones.
    pub removed: Vec<Stamped<FolderId>>,
    /// The folder order, when it changed.
    pub order: Option<Stamped<FolderOrder>>,
    /// Saved-message registers (ADR-0065 section 4): saves and removals.
    pub saved: Vec<Stamped<SavedMessage>>,
}

/// A value with the hybrid-logical-clock stamp it was written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamped<T> {
    /// The stamp.
    pub hlc: u64,
    /// The value.
    pub value: T,
}

impl FolderChanges {
    /// How many registers the change set carries.
    #[must_use]
    pub fn register_count(&self) -> usize {
        self.metas.len()
            + self.members.len()
            + self.pins.len()
            + self.removed.len()
            + usize::from(self.order.is_some())
            + self.saved.len()
    }

    /// `true` when nothing is carried.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.register_count() == 0
    }

    /// Check the register cap and every register.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.register_count() > FOLDER_CHANGES_MAX_REGISTERS {
            return Err(BoundsError::TooManyRegisters);
        }
        self.stamped_entries()
            .iter()
            .try_for_each(|(_, e)| e.validate())
    }

    /// Every register as a stamped [`SyncEntry`].
    #[must_use]
    pub fn stamped_entries(&self) -> Vec<(u64, SyncEntry)> {
        let mut out = Vec::with_capacity(self.register_count());
        out.extend(
            self.metas
                .iter()
                .map(|s| (s.hlc, SyncEntry::FolderMeta(s.value.clone()))),
        );
        out.extend(
            self.members
                .iter()
                .map(|s| (s.hlc, SyncEntry::FolderMember(s.value.clone()))),
        );
        out.extend(
            self.pins
                .iter()
                .map(|s| (s.hlc, SyncEntry::FolderPins(s.value.clone()))),
        );
        out.extend(
            self.removed
                .iter()
                .map(|s| (s.hlc, SyncEntry::FolderRemoved(s.value))),
        );
        if let Some(o) = &self.order {
            out.push((o.hlc, SyncEntry::FolderOrder(o.value.clone())));
        }
        out.extend(
            self.saved
                .iter()
                .map(|s| (s.hlc, SyncEntry::Saved(s.value.clone()))),
        );
        out
    }

    /// Add one stamped folder or saved-message entry to the matching list.
    /// Returns `false` (and adds nothing) for an entry of one of the four
    /// older kinds, or for a second order.
    pub fn push(&mut self, hlc: u64, entry: SyncEntry) -> bool {
        match entry {
            SyncEntry::FolderMeta(value) => self.metas.push(Stamped { hlc, value }),
            SyncEntry::FolderMember(value) => self.members.push(Stamped { hlc, value }),
            SyncEntry::FolderPins(value) => self.pins.push(Stamped { hlc, value }),
            SyncEntry::FolderRemoved(value) => self.removed.push(Stamped { hlc, value }),
            SyncEntry::FolderOrder(value) => {
                if self.order.is_some() {
                    return false;
                }
                self.order = Some(Stamped { hlc, value });
            },
            SyncEntry::Saved(value) => self.saved.push(Stamped { hlc, value }),
            _ => return false,
        }
        true
    }
}

/// One entry of a snapshot's per-conversation list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationEntry {
    /// Flags.
    Flags(FlagsEntry),
    /// A read cursor.
    ReadCursor(ReadCursorEntry),
    /// A draft.
    Draft(DraftEntry),
}

impl From<ConversationEntry> for SyncEntry {
    fn from(e: ConversationEntry) -> Self {
        match e {
            ConversationEntry::Flags(f) => Self::Flags(f),
            ConversationEntry::ReadCursor(c) => Self::ReadCursor(c),
            ConversationEntry::Draft(d) => Self::Draft(d),
        }
    }
}

/// One part of the catch-up re-send (ADR-0058 section 5): folded state,
/// every entry with its ORIGINAL stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSnapshot {
    /// Per-conversation flags, cursors and drafts.
    pub entries: Vec<Stamped<ConversationEntry>>,
    /// The explicit conversation order, when held (and in this part).
    pub conversation_order: Option<Stamped<ConversationOrderEntry>>,
    /// Folder registers, and saved-message registers.
    pub folders: FolderChanges,
    /// 0-based part index.
    pub part: u32,
    /// Number of parts, 1..=64.
    pub parts: u32,
}

impl StateSnapshot {
    /// Check the part index and count and the folder registers.
    ///
    /// # Errors
    ///
    /// The first [`BoundsError`] that applies.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.parts == 0 || self.parts > SNAPSHOT_MAX_PARTS || self.part >= self.parts {
            return Err(BoundsError::SnapshotPart);
        }
        self.folders.validate()
    }

    /// Every entry the part carries, stamped.
    #[must_use]
    pub fn stamped_entries(&self) -> Vec<(u64, SyncEntry)> {
        let mut out: Vec<(u64, SyncEntry)> = self
            .entries
            .iter()
            .map(|s| (s.hlc, s.value.clone().into()))
            .collect();
        if let Some(o) = &self.conversation_order {
            out.push((o.hlc, SyncEntry::ConversationOrder(o.value.clone())));
        }
        out.extend(self.folders.stamped_entries());
        out
    }
}

// ---------------------------------------------------------------------------
// Codec helpers.
// ---------------------------------------------------------------------------

fn put_len(out: &mut Vec<u8>, n: usize) {
    // Every in-memory collection this crate encodes is far below u32::MAX;
    // saturating keeps the function total rather than panicking.
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_bytes32(out: &mut Vec<u8>, b: &[u8]) {
    put_len(out, b.len());
    out.extend_from_slice(b);
}

fn put_ref(out: &mut Vec<u8>, r: &ConversationRef) {
    out.push(r.kind.to_wire());
    // A validated ref id is at most 64 bytes.
    out.push(u8::try_from(r.id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(&r.id);
}

pub(crate) fn take_u8(cur: &mut &[u8]) -> Result<u8, CodecError> {
    let (&b, rest) = cur.split_first().ok_or(CodecError::Truncated)?;
    *cur = rest;
    Ok(b)
}

pub(crate) fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8], CodecError> {
    if cur.len() < n {
        return Err(CodecError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

pub(crate) fn take_u32(cur: &mut &[u8]) -> Result<u32, CodecError> {
    let b = take_n(cur, 4)?;
    let mut a = [0u8; 4];
    a.copy_from_slice(b);
    Ok(u32::from_be_bytes(a))
}

pub(crate) fn take_u64(cur: &mut &[u8]) -> Result<u64, CodecError> {
    let b = take_n(cur, 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Ok(u64::from_be_bytes(a))
}

fn take_bool(cur: &mut &[u8]) -> Result<bool, CodecError> {
    match take_u8(cur)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CodecError::BadBool),
    }
}

fn take_fid(cur: &mut &[u8]) -> Result<FolderId, CodecError> {
    Ok(folder_id_from_slice(take_n(cur, FOLDER_ID_LEN)?)?)
}

pub(crate) fn take_bytes32(cur: &mut &[u8]) -> Result<Vec<u8>, CodecError> {
    let n = take_u32(cur)? as usize;
    Ok(take_n(cur, n)?.to_vec())
}

fn take_string(cur: &mut &[u8]) -> Result<String, CodecError> {
    String::from_utf8(take_bytes32(cur)?).map_err(|_| CodecError::NotUtf8)
}

/// The body of a saved-message entry, after its tag. Every length is
/// checked against its bound before the bytes are copied, and the blob-id
/// count before the list is allocated.
fn take_saved(cur: &mut &[u8]) -> Result<SavedMessage, CodecError> {
    let conversation = take_ref(cur)?;
    let message_id = take_u64(cur)?;
    let value = match take_u8(cur)? {
        SAVED_STATE_PRESENT => {
            let saved_at_ms = take_u64(cur)?;
            let author =
                take_bounded_string(cur, SAVED_AUTHOR_MAX_BYTES, BoundsError::SavedAuthorTooLong)?;
            let excerpt = take_bounded_string(
                cur,
                SAVED_EXCERPT_MAX_BYTES,
                BoundsError::SavedExcerptTooLong,
            )?;
            let attachment_kind = take_bounded_string(
                cur,
                SAVED_ATTACHMENT_KIND_MAX_BYTES,
                BoundsError::SavedAttachmentKindTooLong,
            )?;
            let n = take_u32(cur)? as usize;
            if n > SAVED_BLOB_IDS_MAX {
                return Err(BoundsError::TooManyBlobIds.into());
            }
            let mut blob_ids = Vec::with_capacity(n);
            for _ in 0..n {
                blob_ids.push(blob_id_from_slice(take_n(cur, SAVED_BLOB_ID_LEN)?)?);
            }
            SavedValue::Present {
                saved_at_ms,
                snapshot: SavedSnapshot {
                    author,
                    excerpt,
                    attachment_kind,
                },
                blob_ids,
            }
        },
        SAVED_STATE_REMOVED => SavedValue::Removed,
        _ => return Err(BoundsError::UnknownSavedState.into()),
    };
    Ok(SavedMessage {
        conversation,
        message_id,
        value,
    })
}

/// A length-prefixed UTF-8 string whose length is checked against `max`
/// before a byte of it is copied.
fn take_bounded_string(
    cur: &mut &[u8],
    max: usize,
    too_long: BoundsError,
) -> Result<String, CodecError> {
    let n = take_u32(cur)? as usize;
    if n > max {
        return Err(too_long.into());
    }
    core::str::from_utf8(take_n(cur, n)?)
        .map(str::to_owned)
        .map_err(|_| CodecError::NotUtf8)
}

fn take_ref(cur: &mut &[u8]) -> Result<ConversationRef, CodecError> {
    let kind = ConversationRefKind::from_wire(i64::from(take_u8(cur)?))?;
    let n = usize::from(take_u8(cur)?);
    let id = take_n(cur, n)?.to_vec();
    let r = ConversationRef { kind, id };
    r.validate()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str) -> FolderMeta {
        FolderMeta {
            folder_id: [7; 16],
            name: name.into(),
            include_kinds: INCLUDE_GROUPS,
            exclude_flags: EXCLUDE_ARCHIVED,
        }
    }

    fn sref() -> ConversationRef {
        ConversationRef {
            kind: ConversationRefKind::Group,
            id: vec![5; 32],
        }
    }

    fn saved(message_id: u64, blob_ids: &[BlobId]) -> SavedMessage {
        SavedMessage {
            conversation: sref(),
            message_id,
            value: SavedValue::Present {
                saved_at_ms: 1_700_000_000_000,
                snapshot: SavedSnapshot {
                    author: "Ada".into(),
                    excerpt: "see you at nine".into(),
                    attachment_kind: "photo".into(),
                },
                blob_ids: blob_ids.to_vec(),
            },
        }
    }

    /// `saved(..)` with one field of the value rewritten.
    fn saved_with(f: impl FnOnce(&mut u64, &mut SavedSnapshot, &mut Vec<BlobId>)) -> SavedMessage {
        let mut m = saved(1, &[[1; 32]]);
        if let SavedValue::Present {
            saved_at_ms,
            snapshot,
            blob_ids,
        } = &mut m.value
        {
            f(saved_at_ms, snapshot, blob_ids);
        }
        m
    }

    fn distinct_blob_ids(n: usize) -> Vec<BlobId> {
        (0..n)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                id
            })
            .collect()
    }

    #[test]
    fn every_saved_bound_is_checked_and_the_limits_are_accepted() {
        use BoundsError as B;
        // At the limits.
        let at_limit = saved_with(|_, s, b| {
            s.author = "a".repeat(SAVED_AUTHOR_MAX_BYTES);
            s.excerpt = "e".repeat(SAVED_EXCERPT_MAX_BYTES);
            s.attachment_kind = "k".repeat(SAVED_ATTACHMENT_KIND_MAX_BYTES);
            *b = distinct_blob_ids(SAVED_BLOB_IDS_MAX);
        });
        assert_eq!(at_limit.validate(), Ok(()));
        let e = SyncEntry::Saved(at_limit);
        assert_eq!(SyncEntry::from_canonical_bytes(&e.canonical_bytes()), Ok(e));
        // An empty snapshot and no blob ids: a text message from nobody
        // this device can name.
        let bare = saved_with(|_, s, b| {
            *s = SavedSnapshot::default();
            b.clear();
        });
        assert_eq!(bare.validate(), Ok(()));

        // One past each limit.
        let cases: Vec<(SavedMessage, B)> = vec![
            (
                saved_with(|_, s, _| s.author = "a".repeat(SAVED_AUTHOR_MAX_BYTES + 1)),
                B::SavedAuthorTooLong,
            ),
            (
                saved_with(|_, s, _| s.excerpt = "e".repeat(SAVED_EXCERPT_MAX_BYTES + 1)),
                B::SavedExcerptTooLong,
            ),
            (
                saved_with(|_, s, _| {
                    s.attachment_kind = "k".repeat(SAVED_ATTACHMENT_KIND_MAX_BYTES + 1);
                }),
                B::SavedAttachmentKindTooLong,
            ),
            (
                saved_with(|_, _, b| *b = distinct_blob_ids(SAVED_BLOB_IDS_MAX + 1)),
                B::TooManyBlobIds,
            ),
            (
                saved_with(|_, _, b| *b = vec![[1; 32], [2; 32], [1; 32]]),
                B::DuplicateBlobId,
            ),
            (
                SavedMessage {
                    conversation: ConversationRef {
                        kind: ConversationRefKind::Group,
                        id: vec![],
                    },
                    ..saved(1, &[])
                },
                B::RefIdLength,
            ),
            (
                SavedMessage {
                    conversation: ConversationRef {
                        kind: ConversationRefKind::Channel,
                        id: vec![0; CONVERSATION_REF_ID_MAX_LEN + 1],
                    },
                    value: SavedValue::Removed,
                    message_id: 1,
                },
                B::RefIdLength,
            ),
        ];
        for (i, (m, want)) in cases.into_iter().enumerate() {
            assert_eq!(m.validate(), Err(want), "case {i}");
            assert_eq!(SyncEntry::Saved(m).validate(), Err(want), "case {i}");
        }
        assert_eq!(blob_id_from_slice(&[0; 31]), Err(B::BlobIdLength));
        assert_eq!(blob_id_from_slice(&[0; 33]), Err(B::BlobIdLength));
        assert_eq!(blob_id_from_slice(&[7; 32]), Ok([7; 32]));
    }

    /// The canonical bytes of a saved register up to and including its
    /// state byte.
    fn saved_prefix(state: u8) -> Vec<u8> {
        let mut b = vec![TAG_SAVED];
        put_ref(&mut b, &sref());
        b.extend_from_slice(&9u64.to_be_bytes());
        b.push(state);
        b
    }

    #[test]
    fn the_canonical_decoder_refuses_a_saved_bound_before_it_copies_anything() {
        use BoundsError as B;
        // Each case declares a length past its bound and then supplies NO
        // bytes for it: the answer is the bound, not `Truncated`, so the
        // length was checked before the bytes were looked at.
        let present = || {
            let mut b = saved_prefix(SAVED_STATE_PRESENT);
            b.extend_from_slice(&0u64.to_be_bytes());
            b
        };
        let len = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes();

        let mut author = present();
        author.extend_from_slice(&len(SAVED_AUTHOR_MAX_BYTES + 1));
        assert_eq!(
            SyncEntry::from_canonical_bytes(&author),
            Err(CodecError::Bounds(B::SavedAuthorTooLong))
        );

        let mut excerpt = present();
        excerpt.extend_from_slice(&len(0));
        excerpt.extend_from_slice(&len(SAVED_EXCERPT_MAX_BYTES + 1));
        assert_eq!(
            SyncEntry::from_canonical_bytes(&excerpt),
            Err(CodecError::Bounds(B::SavedExcerptTooLong))
        );

        let mut kind = present();
        kind.extend_from_slice(&len(0));
        kind.extend_from_slice(&len(0));
        kind.extend_from_slice(&len(SAVED_ATTACHMENT_KIND_MAX_BYTES + 1));
        assert_eq!(
            SyncEntry::from_canonical_bytes(&kind),
            Err(CodecError::Bounds(B::SavedAttachmentKindTooLong))
        );

        let mut blobs = present();
        blobs.extend_from_slice(&len(0));
        blobs.extend_from_slice(&len(0));
        blobs.extend_from_slice(&len(0));
        blobs.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            SyncEntry::from_canonical_bytes(&blobs),
            Err(CodecError::Bounds(B::TooManyBlobIds))
        );

        // A repeated blob id, a state this build does not know (0
        // included), text that is not UTF-8, a short blob id, and bytes
        // after a tombstone.
        let dup = SyncEntry::Saved(saved_with(|_, _, b| *b = vec![[3; 32], [3; 32]]));
        assert_eq!(
            SyncEntry::from_canonical_bytes(&dup.canonical_bytes()),
            Err(CodecError::Bounds(B::DuplicateBlobId))
        );
        for state in [0u8, 3, 255] {
            assert_eq!(
                SyncEntry::from_canonical_bytes(&saved_prefix(state)),
                Err(CodecError::Bounds(B::UnknownSavedState)),
                "state {state}"
            );
        }
        let mut not_utf8 = present();
        not_utf8.extend_from_slice(&len(1));
        not_utf8.push(0xFF);
        assert_eq!(
            SyncEntry::from_canonical_bytes(&not_utf8),
            Err(CodecError::NotUtf8)
        );
        let mut short_blob = present();
        short_blob.extend_from_slice(&len(0));
        short_blob.extend_from_slice(&len(0));
        short_blob.extend_from_slice(&len(0));
        short_blob.extend_from_slice(&len(1));
        short_blob.extend_from_slice(&[1; 31]);
        assert_eq!(
            SyncEntry::from_canonical_bytes(&short_blob),
            Err(CodecError::Truncated)
        );
        let mut tombstone = saved_prefix(SAVED_STATE_REMOVED);
        assert!(SyncEntry::from_canonical_bytes(&tombstone).is_ok());
        tombstone.push(0);
        assert_eq!(
            SyncEntry::from_canonical_bytes(&tombstone),
            Err(CodecError::Trailing)
        );
    }

    #[test]
    fn a_removal_outranks_a_save_of_the_same_message_in_canonical_bytes() {
        // What decides an exact stamp tie: whatever the save carries, the
        // two encodings share the key and differ first at the state byte.
        let present = SyncEntry::Saved(saved_with(|at, s, b| {
            *at = u64::MAX;
            s.author = "\u{10FFFF}".repeat(60);
            *b = vec![[0xFF; 32]];
        }));
        let removed = SyncEntry::Saved(SavedMessage {
            conversation: sref(),
            message_id: 1,
            value: SavedValue::Removed,
        });
        assert_eq!(present.key(), removed.key());
        assert!(removed.canonical_bytes() > present.canonical_bytes());
    }

    #[test]
    fn composing_a_save_cuts_the_text_on_a_character_boundary_and_names_each_blob_once() {
        // 4-byte scalars: 512 bytes is exactly 128 of them, 255 is not a
        // multiple of 4 and must round down to 252.
        let long = "😀".repeat(200);
        let snap = SavedSnapshot::fitted(&long, &long, "voice");
        assert_eq!(snap.excerpt.len(), SAVED_EXCERPT_MAX_BYTES);
        assert_eq!(snap.author.len(), 252);
        assert_eq!(snap.attachment_kind, "voice");
        assert!(long.starts_with(&snap.excerpt));
        assert_eq!(snap.validate(), Ok(()));
        // Short text is untouched.
        assert_eq!(SavedSnapshot::fitted("Ada", "hi", "").excerpt, "hi");

        let m = SavedMessage::present(
            sref(),
            4,
            99,
            &SavedSnapshot {
                author: long.clone(),
                excerpt: long,
                attachment_kind: "voice".into(),
            },
            &[vec![1u8; 32], vec![2; 32], vec![1; 32]],
        );
        let Ok(m) = m else {
            panic!("a well-formed save was refused");
        };
        assert_eq!(m.validate(), Ok(()));
        assert!(matches!(
            &m.value,
            SavedValue::Present { blob_ids, saved_at_ms: 99, .. } if *blob_ids == vec![[1; 32], [2; 32]]
        ));

        // What composing refuses rather than repairs.
        let no_blobs: [Vec<u8>; 0] = [];
        assert_eq!(
            SavedMessage::present(sref(), 1, 1, &SavedSnapshot::default(), &[vec![1u8; 31]]),
            Err(BoundsError::BlobIdLength)
        );
        assert_eq!(
            SavedMessage::present(
                sref(),
                1,
                1,
                &SavedSnapshot::default(),
                &distinct_blob_ids(SAVED_BLOB_IDS_MAX + 1)
            ),
            Err(BoundsError::TooManyBlobIds)
        );
        assert_eq!(
            SavedMessage::present(
                sref(),
                1,
                1,
                &SavedSnapshot {
                    attachment_kind: "k".repeat(SAVED_ATTACHMENT_KIND_MAX_BYTES + 1),
                    ..SavedSnapshot::default()
                },
                &no_blobs
            ),
            Err(BoundsError::SavedAttachmentKindTooLong)
        );
        let bad_ref = ConversationRef {
            kind: ConversationRefKind::OneToOne,
            id: vec![],
        };
        assert_eq!(
            SavedMessage::removed(bad_ref, 1),
            Err(BoundsError::RefIdLength)
        );
        assert!(SavedMessage::removed(sref(), 1).is_ok());
    }

    #[test]
    fn a_change_set_carries_saved_registers_like_any_other() {
        let mut c = FolderChanges::default();
        assert!(c.push(5, SyncEntry::Saved(saved(1, &[[1; 32]]))));
        assert!(c.push(
            6,
            SyncEntry::Saved(SavedMessage {
                conversation: sref(),
                message_id: 2,
                value: SavedValue::Removed,
            })
        ));
        assert!(!c.is_empty());
        assert_eq!(c.register_count(), 2);
        assert_eq!(c.validate(), Ok(()));
        let stamps: Vec<u64> = c.stamped_entries().iter().map(|(h, _)| *h).collect();
        assert_eq!(stamps, vec![5, 6]);
        // An out-of-bounds saved register refuses the whole change set.
        c.saved.push(Stamped {
            hlc: 7,
            value: saved_with(|_, s, _| s.excerpt = "e".repeat(SAVED_EXCERPT_MAX_BYTES + 1)),
        });
        assert_eq!(c.validate(), Err(BoundsError::SavedExcerptTooLong));
        // Saved registers count against the register cap.
        let mut full = FolderChanges::default();
        for i in 0..=FOLDER_CHANGES_MAX_REGISTERS {
            full.saved.push(Stamped {
                hlc: 1,
                value: SavedMessage {
                    conversation: sref(),
                    message_id: i as u64,
                    value: SavedValue::Removed,
                },
            });
        }
        assert_eq!(full.validate(), Err(BoundsError::TooManyRegisters));
    }

    #[test]
    fn a_folder_name_follows_the_bounds() {
        assert!(check_folder_name("Work").is_ok());
        assert!(check_folder_name("🏠 Home").is_ok());
        assert_eq!(check_folder_name(""), Err(BoundsError::NameEmpty));
        assert!(check_folder_name(&"a".repeat(24)).is_ok());
        assert_eq!(
            check_folder_name(&"a".repeat(25)),
            Err(BoundsError::NameTooLong)
        );
        // 24 scalars of four bytes each is 96 bytes: at the limit.
        assert!(check_folder_name(&"😀".repeat(24)).is_ok());
        assert_eq!(check_folder_name("a\nb"), Err(BoundsError::NameCharacters));
        assert_eq!(check_folder_name(" Work"), Err(BoundsError::NameCharacters));
        assert_eq!(
            check_folder_name("a\u{202E}b"),
            Err(BoundsError::NameCharacters)
        );
    }

    #[test]
    fn unknown_bits_are_refused_not_read_as_off() {
        let mut m = meta("Work");
        m.include_kinds = 0b1000;
        assert_eq!(m.validate(), Err(BoundsError::UnknownBits));
        let mut m = meta("Work");
        m.exclude_flags = 1 << 31;
        assert_eq!(m.validate(), Err(BoundsError::UnknownBits));
    }

    #[test]
    fn every_kind_round_trips_through_the_canonical_encoding() {
        let r = ConversationRef {
            kind: ConversationRefKind::Channel,
            id: vec![1, 2, 3],
        };
        let entries = vec![
            SyncEntry::FolderMeta(meta("Work")),
            SyncEntry::FolderMember(FolderMember {
                folder_id: [1; 16],
                conversation: r.clone(),
                state: MemberState::Excluded,
            }),
            SyncEntry::FolderPins(FolderPins {
                folder_id: [1; 16],
                pinned: vec![r],
            }),
            SyncEntry::FolderRemoved([3; 16]),
            SyncEntry::FolderOrder(FolderOrder {
                folder_ids: vec![[1; 16], [2; 16]],
            }),
            SyncEntry::Flags(FlagsEntry {
                group_id: vec![9; 32],
                pinned: true,
                muted: false,
                archived: true,
                unread_override: false,
            }),
            SyncEntry::ReadCursor(ReadCursorEntry {
                group_id: vec![9; 32],
                up_to_message_id: vec![4, 5],
                read_at_ms: 77,
            }),
            SyncEntry::Draft(DraftEntry {
                group_id: vec![9; 32],
                text: "hi".into(),
                reply_to_id: vec![],
            }),
            SyncEntry::ConversationOrder(ConversationOrderEntry {
                group_ids: vec![vec![1], vec![2, 2]],
            }),
            SyncEntry::Saved(saved(7, &[[1; 32], [2; 32]])),
            SyncEntry::Saved(saved(8, &[])),
            SyncEntry::Saved(SavedMessage {
                conversation: sref(),
                message_id: u64::MAX,
                value: SavedValue::Removed,
            }),
        ];
        for e in entries {
            let bytes = e.canonical_bytes();
            assert_eq!(SyncEntry::from_canonical_bytes(&bytes), Ok(e));
        }
    }

    #[test]
    fn a_malformed_canonical_entry_is_refused() {
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[]),
            Err(CodecError::Truncated)
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[99]),
            Err(CodecError::UnknownTag)
        );
        let mut b = SyncEntry::FolderRemoved([1; 16]).canonical_bytes();
        b.push(0);
        assert_eq!(
            SyncEntry::from_canonical_bytes(&b),
            Err(CodecError::Trailing)
        );
    }

    #[test]
    fn change_set_register_cap() {
        let mut c = FolderChanges::default();
        for i in 0..=FOLDER_CHANGES_MAX_REGISTERS {
            c.removed.push(Stamped {
                hlc: i as u64,
                value: [1; 16],
            });
        }
        assert_eq!(c.validate(), Err(BoundsError::TooManyRegisters));
        c.removed.pop();
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn snapshot_part_bounds() {
        let s = |part, parts| StateSnapshot {
            entries: vec![],
            conversation_order: None,
            folders: FolderChanges::default(),
            part,
            parts,
        };
        assert!(s(0, 1).validate().is_ok());
        assert!(s(63, 64).validate().is_ok());
        assert_eq!(s(1, 1).validate(), Err(BoundsError::SnapshotPart));
        assert_eq!(s(0, 0).validate(), Err(BoundsError::SnapshotPart));
        assert_eq!(s(0, 65).validate(), Err(BoundsError::SnapshotPart));
    }
}
