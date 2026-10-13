// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The registers the account's devices keep in step (ADR-0058).
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
/// A lineage register's group id maximum length in bytes (each of the two
/// ids; an MLS group id is 32 bytes today and travels as opaque bytes).
pub const LINEAGE_GROUP_ID_MAX_LEN: usize = 64;
/// Shortest recovery email address the register holds, in bytes (`a@b`).
/// An empty address is the removal and is not subject to it.
pub const RECOVERY_EMAIL_MIN_LEN: usize = 3;
/// Longest recovery email address the register holds, in bytes (the RFC
/// 5321 path limit).
pub const RECOVERY_EMAIL_MAX_LEN: usize = 254;

/// Longest own username the register holds, in bytes: the canonical
/// username bound (`lakko_contacts::username::USERNAME_MAX_LEN`). An empty
/// value is "no username".
pub const OWN_USERNAME_MAX_LEN: usize = lakko_contacts::username::USERNAME_MAX_LEN;

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

/// Why an entry, a change set or a snapshot was refused (ADR-0058 section 4).
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
    /// A lineage register's successor or root id was empty or longer than
    /// 64 bytes.
    #[error("lineage group id length is out of bounds")]
    LineageIdLength,
    /// A lineage register named a group as its own root. The root of a
    /// lineage has no register: it is the conversation id itself.
    #[error("lineage register names a group as its own successor")]
    LineageSelfReference,
    /// A recovery email register's address was neither empty nor 3..=254
    /// bytes long.
    #[error("recovery email address length is out of bounds")]
    RecoveryEmailLength,
    /// A recovery email register's address held a control character or
    /// white space, did not hold exactly one `@`, or had nothing on one side
    /// of it.
    #[error("recovery email address is not a normalised address")]
    RecoveryEmailShape,
    /// An own-username register's value was neither empty nor a canonical
    /// username (`[a-z0-9_]`, 3..=32).
    #[error("own username is not a canonical username")]
    OwnUsername,
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

/// One conversation-lineage register (ADR-0073 R.5): the group
/// `successor_group_id` continues the conversation whose id is
/// `root_conversation_id` (the group id of the FIRST group of the lineage).
///
/// A device that was in the old group learns the lineage from the claim
/// posted there; a device added later, and a restore, cannot read the old
/// group and learn it from this register instead. Without it such a device
/// shows the conversation under the successor's id and the person's folder,
/// pin and mute for it do not apply.
///
/// The register is keyed on the successor, so one conversation may have
/// several successors over time and every one of them names the same root.
/// A root has no register of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageEntry {
    /// The MLS group id of the group that continues the conversation.
    pub successor_group_id: Vec<u8>,
    /// The conversation id: the group id of the lineage's first group.
    pub root_conversation_id: Vec<u8>,
}

impl LineageEntry {
    /// Check both ids are 1..=[`LINEAGE_GROUP_ID_MAX_LEN`] bytes and differ.
    ///
    /// # Errors
    ///
    /// [`BoundsError::LineageIdLength`] or
    /// [`BoundsError::LineageSelfReference`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        let ok = |id: &[u8]| !id.is_empty() && id.len() <= LINEAGE_GROUP_ID_MAX_LEN;
        if !ok(&self.successor_group_id) || !ok(&self.root_conversation_id) {
            return Err(BoundsError::LineageIdLength);
        }
        if self.successor_group_id == self.root_conversation_id {
            return Err(BoundsError::LineageSelfReference);
        }
        Ok(())
    }
}

/// The account's recovery email address (ADR-0075, amended 2026-10-13): one
/// register per account.
///
/// `address` is the NORMALISED address
/// (`lakko_mnemonic::recovery_email::normalize_recovery_email`), or empty for
/// "no recovery email". A removal is a stamped empty value and is kept like
/// any other value, so a device that was offline across the removal cannot
/// bring the address back with its older stamp.
///
/// The address never leaves the account's own devices and its phrase-sealed
/// backup; the server holds a keyed hash of it and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEmailEntry {
    /// The normalised address, or empty when the account has none.
    pub address: String,
}

/// Check a recovery email register's address: empty (the removal), or a
/// normalised address.
///
/// This crate does not depend on `lakko-mnemonic` (it would pull the BIP39
/// tables into every crate that folds synced state, the notification
/// extension among them), so the check here is structural: 3..=254 bytes, no
/// control character, no white space, exactly one `@` with text on both
/// sides. Lower-casing is the writer's job (the FFI surfaces normalise with
/// the one definition before they write); a register is never re-spelled on
/// receipt, because two devices re-spelling differently would not converge.
///
/// # Errors
///
/// [`BoundsError::RecoveryEmailLength`] or
/// [`BoundsError::RecoveryEmailShape`].
pub fn check_recovery_email_address(address: &str) -> Result<(), BoundsError> {
    if address.is_empty() {
        return Ok(());
    }
    if address.len() < RECOVERY_EMAIL_MIN_LEN || address.len() > RECOVERY_EMAIL_MAX_LEN {
        return Err(BoundsError::RecoveryEmailLength);
    }
    if address.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(BoundsError::RecoveryEmailShape);
    }
    let mut parts = address.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) if !local.is_empty() && !domain.is_empty() => Ok(()),
        _ => Err(BoundsError::RecoveryEmailShape),
    }
}

impl RecoveryEmailEntry {
    /// The stamped removal: no recovery email.
    #[must_use]
    pub const fn removed() -> Self {
        Self {
            address: String::new(),
        }
    }

    /// `true` when the register says the account has no recovery email.
    #[must_use]
    pub fn is_removed(&self) -> bool {
        self.address.is_empty()
    }

    /// [`check_recovery_email_address`] of the held address.
    ///
    /// # Errors
    ///
    /// [`BoundsError::RecoveryEmailLength`] or
    /// [`BoundsError::RecoveryEmailShape`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        check_recovery_email_address(&self.address)
    }
}

/// The account's own username (ADR-0076 section 3): one register per
/// account.
///
/// `username` is the CANONICAL username
/// ([`lakko_contacts::username::check_canonical_username`]: `[a-z0-9_]`,
/// 3..=32), or empty for "no username". An empty value is stamped and kept
/// like any other, so a device that was offline across the change cannot
/// bring an old name back with its older stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnUsernameEntry {
    /// The canonical username, or empty when the account has none.
    pub username: String,
}

/// Check an own-username register's value: empty (no username), or a
/// canonical username by the one rule
/// ([`lakko_contacts::username::check_canonical_username`]).
///
/// # Errors
///
/// [`BoundsError::OwnUsername`].
pub fn check_own_username(username: &str) -> Result<(), BoundsError> {
    if username.is_empty() {
        return Ok(());
    }
    lakko_contacts::username::check_canonical_username(username)
        .map_err(|_| BoundsError::OwnUsername)
}

impl OwnUsernameEntry {
    /// The stamped "no username".
    #[must_use]
    pub const fn none() -> Self {
        Self {
            username: String::new(),
        }
    }

    /// `true` when the register says the account has no username.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.username.is_empty()
    }

    /// [`check_own_username`] of the held value.
    ///
    /// # Errors
    ///
    /// [`BoundsError::OwnUsername`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        check_own_username(&self.username)
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
    /// A conversation-lineage register (ADR-0073 R.5).
    Lineage(LineageEntry),
    /// The account's recovery email address (ADR-0075).
    RecoveryEmail(RecoveryEmailEntry),
    /// The account's own username (ADR-0076). Appended last.
    OwnUsername(OwnUsernameEntry),
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
    /// The lineage register of this successor group. Appended after the
    /// ADR-0058 kinds, so their register order is unchanged.
    Lineage(Vec<u8>),
    /// The account's one recovery email register. Appended after the
    /// older kinds, so their register order is unchanged.
    RecoveryEmail,
    /// The account's one own-username register. Appended last, so the
    /// register order of every older kind is unchanged.
    OwnUsername,
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
const TAG_LINEAGE: u8 = 10;
const TAG_RECOVERY_EMAIL: u8 = 11;
const TAG_OWN_USERNAME: u8 = 12;

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
            Self::Lineage(l) => RegisterKey::Lineage(l.successor_group_id.clone()),
            Self::RecoveryEmail(_) => RegisterKey::RecoveryEmail,
            Self::OwnUsername(_) => RegisterKey::OwnUsername,
        }
    }

    /// Check the ADR-0058 section 4 bounds that apply to this one entry.
    /// The four older conversation kinds carry no bounds of their own.
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
            Self::Lineage(l) => l.validate(),
            Self::RecoveryEmail(r) => r.validate(),
            Self::OwnUsername(u) => u.validate(),
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
            Self::Lineage(l) => {
                out.push(TAG_LINEAGE);
                put_bytes32(&mut out, &l.successor_group_id);
                put_bytes32(&mut out, &l.root_conversation_id);
            },
            Self::RecoveryEmail(r) => {
                out.push(TAG_RECOVERY_EMAIL);
                put_bytes32(&mut out, r.address.as_bytes());
            },
            Self::OwnUsername(u) => {
                out.push(TAG_OWN_USERNAME);
                put_bytes32(&mut out, u.username.as_bytes());
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
            TAG_LINEAGE => Self::Lineage(take_lineage(&mut cur)?),
            TAG_RECOVERY_EMAIL => Self::RecoveryEmail(take_recovery_email(&mut cur)?),
            TAG_OWN_USERNAME => Self::OwnUsername(take_own_username(&mut cur)?),
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
    /// An entry broke an ADR-0058 bound.
    #[error("synced entry out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

/// A live folder edit: only the registers that changed, each with its
/// own stamp.
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
        out
    }

    /// Add one stamped folder entry to the matching list. Returns `false`
    /// (and adds nothing) for a non-folder entry, or for a second order.
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
    /// Folder registers.
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

/// A lineage id: the length is checked against the bound BEFORE the bytes
/// are taken, so a hostile length never sizes a copy.
fn take_lineage_id(cur: &mut &[u8]) -> Result<Vec<u8>, CodecError> {
    let n = take_u32(cur)? as usize;
    if n == 0 || n > LINEAGE_GROUP_ID_MAX_LEN {
        return Err(BoundsError::LineageIdLength.into());
    }
    Ok(take_n(cur, n)?.to_vec())
}

fn take_lineage(cur: &mut &[u8]) -> Result<LineageEntry, CodecError> {
    let successor_group_id = take_lineage_id(cur)?;
    let root_conversation_id = take_lineage_id(cur)?;
    Ok(LineageEntry {
        successor_group_id,
        root_conversation_id,
    })
}

/// A recovery email register: the length is checked against the bound BEFORE
/// the bytes are taken, so a hostile length never sizes a copy.
fn take_recovery_email(cur: &mut &[u8]) -> Result<RecoveryEmailEntry, CodecError> {
    let n = take_u32(cur)? as usize;
    if n > RECOVERY_EMAIL_MAX_LEN {
        return Err(BoundsError::RecoveryEmailLength.into());
    }
    let address = String::from_utf8(take_n(cur, n)?.to_vec()).map_err(|_| CodecError::NotUtf8)?;
    Ok(RecoveryEmailEntry { address })
}

/// An own-username register: the length is checked against the bound BEFORE
/// the bytes are taken, so a hostile length never sizes a copy.
fn take_own_username(cur: &mut &[u8]) -> Result<OwnUsernameEntry, CodecError> {
    let n = take_u32(cur)? as usize;
    if n > OWN_USERNAME_MAX_LEN {
        return Err(BoundsError::OwnUsername.into());
    }
    let username = String::from_utf8(take_n(cur, n)?.to_vec()).map_err(|_| CodecError::NotUtf8)?;
    Ok(OwnUsernameEntry { username })
}

fn take_string(cur: &mut &[u8]) -> Result<String, CodecError> {
    String::from_utf8(take_bytes32(cur)?).map_err(|_| CodecError::NotUtf8)
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
            SyncEntry::Lineage(LineageEntry {
                successor_group_id: vec![7; 32],
                root_conversation_id: vec![8; 32],
            }),
            SyncEntry::RecoveryEmail(RecoveryEmailEntry {
                address: "änna@büro.example".into(),
            }),
            SyncEntry::RecoveryEmail(RecoveryEmailEntry::removed()),
            SyncEntry::OwnUsername(OwnUsernameEntry {
                username: "bob_42".into(),
            }),
            SyncEntry::OwnUsername(OwnUsernameEntry::none()),
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
    fn a_lineage_register_follows_its_bounds_and_its_layout_is_pinned() {
        let l = |s: Vec<u8>, r: Vec<u8>| LineageEntry {
            successor_group_id: s,
            root_conversation_id: r,
        };
        assert_eq!(l(vec![1], vec![2]).validate(), Ok(()));
        assert_eq!(l(vec![1; 64], vec![2; 64]).validate(), Ok(()));
        for (s, r) in [
            (vec![], vec![2]),
            (vec![1], vec![]),
            (vec![1; 65], vec![2]),
            (vec![1], vec![2; 65]),
        ] {
            assert_eq!(l(s, r).validate(), Err(BoundsError::LineageIdLength));
        }
        assert_eq!(
            l(vec![1, 2], vec![1, 2]).validate(),
            Err(BoundsError::LineageSelfReference)
        );

        let bytes = SyncEntry::Lineage(l(vec![0xAA, 0xBB], vec![0xCC])).canonical_bytes();
        assert_eq!(bytes, [10, 0, 0, 0, 2, 0xAA, 0xBB, 0, 0, 0, 1, 0xCC]);

        // Decode refuses what validate refuses, and a hostile length.
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[10, 0, 0, 0, 1, 5, 0, 0, 0, 1, 5]),
            Err(CodecError::Bounds(BoundsError::LineageSelfReference))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[10, 0, 0, 0, 0, 0, 0, 0, 1, 5]),
            Err(CodecError::Bounds(BoundsError::LineageIdLength))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[10, 0xFF, 0xFF, 0xFF, 0xFF, 1]),
            Err(CodecError::Bounds(BoundsError::LineageIdLength))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[10, 0, 0, 0, 4, 1]),
            Err(CodecError::Truncated)
        );
    }

    #[test]
    fn a_lineage_register_never_enters_a_folder_change_set() {
        let mut c = FolderChanges::default();
        assert!(!c.push(
            1,
            SyncEntry::Lineage(LineageEntry {
                successor_group_id: vec![1],
                root_conversation_id: vec![2],
            })
        ));
        assert!(c.is_empty());
    }

    #[test]
    fn a_recovery_email_register_follows_its_bounds_and_its_layout_is_pinned() {
        let ok = check_recovery_email_address;
        assert_eq!(ok(""), Ok(()));
        assert_eq!(ok("a@b"), Ok(()));
        let longest = format!("{}@b", "a".repeat(RECOVERY_EMAIL_MAX_LEN - 2));
        assert_eq!(longest.len(), 254);
        assert_eq!(ok(&longest), Ok(()));
        assert_eq!(
            ok(&format!("a{longest}")),
            Err(BoundsError::RecoveryEmailLength)
        );
        for short in ["a", "a@", "@", "ab"] {
            assert_eq!(ok(short), Err(BoundsError::RecoveryEmailLength));
        }
        for bad in [
            "abc",
            "a@b@c",
            "@ab",
            "ab@",
            "a b@c",
            "a@b\n",
            "a\u{0}@b",
            " a@b",
            "a@b\u{a0}",
        ] {
            assert_eq!(ok(bad), Err(BoundsError::RecoveryEmailShape), "{bad:?}");
        }
        assert!(RecoveryEmailEntry::removed().is_removed());

        let e = |a: &str| {
            SyncEntry::RecoveryEmail(RecoveryEmailEntry {
                address: a.to_owned(),
            })
        };
        assert_eq!(
            e("a@b").canonical_bytes(),
            [11, 0, 0, 0, 3, b'a', b'@', b'b']
        );
        assert_eq!(e("").canonical_bytes(), [11, 0, 0, 0, 0]);
        assert_eq!(e("a@b").key(), RegisterKey::RecoveryEmail);
        assert_eq!(e("").key(), RegisterKey::RecoveryEmail);
        // The register sorts after every older kind.
        assert!(RegisterKey::Lineage(vec![0xFF; 64]) < RegisterKey::RecoveryEmail);

        // Decode refuses what validate refuses, a hostile length, bytes that
        // are not UTF-8, and trailing bytes.
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 2, b'a', b'@']),
            Err(CodecError::Bounds(BoundsError::RecoveryEmailLength))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 3, b'a', b'b', b'c']),
            Err(CodecError::Bounds(BoundsError::RecoveryEmailShape))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 0xFF, b'a']),
            Err(CodecError::Bounds(BoundsError::RecoveryEmailLength))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0xFF, 0xFF, 0xFF, 0xFF, 1]),
            Err(CodecError::Bounds(BoundsError::RecoveryEmailLength))
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 3, b'a', 0xFF, b'b']),
            Err(CodecError::NotUtf8)
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 3, b'a', b'@']),
            Err(CodecError::Truncated)
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[11, 0, 0, 0, 0, 0]),
            Err(CodecError::Trailing)
        );
    }

    #[test]
    fn a_recovery_email_register_never_enters_a_folder_change_set() {
        let mut c = FolderChanges::default();
        assert!(!c.push(
            1,
            SyncEntry::RecoveryEmail(RecoveryEmailEntry {
                address: "a@b".into(),
            })
        ));
        assert!(c.is_empty());
    }

    #[test]
    fn an_own_username_register_follows_its_bounds_and_its_layout_is_pinned() {
        let ok = check_own_username;
        assert_eq!(OWN_USERNAME_MAX_LEN, 32);
        assert_eq!(ok(""), Ok(()));
        assert_eq!(ok("bob"), Ok(()));
        assert_eq!(ok("a_1"), Ok(()));
        assert_eq!(ok(&"a".repeat(32)), Ok(()));
        for bad in ["a", "ab", "Bob", "bo b", "bob!", "b\u{f6}b", "@bob", " bob"] {
            assert_eq!(ok(bad), Err(BoundsError::OwnUsername), "{bad:?}");
        }
        assert_eq!(ok(&"a".repeat(33)), Err(BoundsError::OwnUsername));
        assert!(OwnUsernameEntry::none().is_none());

        let e = |u: &str| {
            SyncEntry::OwnUsername(OwnUsernameEntry {
                username: u.to_owned(),
            })
        };
        assert_eq!(
            e("bob").canonical_bytes(),
            [12, 0, 0, 0, 3, b'b', b'o', b'b']
        );
        assert_eq!(e("").canonical_bytes(), [12, 0, 0, 0, 0]);
        assert_eq!(e("bob").key(), RegisterKey::OwnUsername);
        assert_eq!(e("").key(), RegisterKey::OwnUsername);
        // The register sorts after every older kind.
        assert!(RegisterKey::RecoveryEmail < RegisterKey::OwnUsername);

        let bounds = Err(CodecError::Bounds(BoundsError::OwnUsername));
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 2, b'a', b'b']),
            bounds
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 3, b'B', b'o', b'b']),
            bounds
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 33, b'a']),
            bounds
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0xFF, 0xFF, 0xFF, 0xFF, 1]),
            bounds
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 3, b'a', 0xFF, b'b']),
            Err(CodecError::NotUtf8)
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 3, b'a', b'b']),
            Err(CodecError::Truncated)
        );
        assert_eq!(
            SyncEntry::from_canonical_bytes(&[12, 0, 0, 0, 0, 0]),
            Err(CodecError::Trailing)
        );
    }

    #[test]
    fn an_own_username_register_never_enters_a_folder_change_set() {
        let mut c = FolderChanges::default();
        assert!(!c.push(
            1,
            SyncEntry::OwnUsername(OwnUsernameEntry {
                username: "bob".into(),
            })
        ));
        assert!(c.is_empty());
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
