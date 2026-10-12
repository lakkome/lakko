// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! State the account's own devices keep in step over the DeviceGroup
//! (ADR-0058): chat folders, and the four older maps (flags, read cursors,
//! drafts, conversation order).
//!
//! ## Where the types live
//!
//! This crate owns plain Rust types and the fold; `lakko-mls` converts the
//! `devicegroup_v1.proto` messages into them when it decodes a DeviceGroup
//! payload, and back when it composes one. So the dependency runs
//! `lakko-mls -> lakko-sync`, never the other way, and this crate needs no
//! protobuf. The bounds of ADR-0058 section 4 are written here once
//! ([`entry::FolderChanges::validate`], [`entry::StateSnapshot::validate`])
//! and the decoder calls them; the display-name character rule comes from
//! `lakko_contacts::profile`, the one definition it has.
//!
//! - [`entry`] — the register types, their bounds and their canonical
//!   encoding (the tie-break and the persistence element).
//! - [`lineage`] — the envelope the conversation-lineage register
//!   (ADR-0065 R.5) travels in between the account's devices.
//! - [`state`] — the fold ([`SyncedState`]), its queries, its persistence
//!   codec, and [`should_send_snapshot`].

pub mod entry;
pub mod lineage;
pub mod state;

pub use entry::{
    check_folder_name, folder_id_from_slice, BoundsError, CodecError, ConversationEntry,
    ConversationOrderEntry, ConversationRef, ConversationRefKind, DraftEntry, FlagsEntry,
    FolderChanges, FolderId, FolderMember, FolderMeta, FolderOrder, FolderPins, LineageEntry,
    MemberState, ReadCursorEntry, RegisterKey, Stamped, StateSnapshot, SyncEntry,
    CONVERSATION_REF_ID_MAX_LEN, EXCLUDE_ARCHIVED, EXCLUDE_MUTED, EXCLUDE_READ,
    FOLDER_CHANGES_MAX_REGISTERS, FOLDER_ID_LEN, FOLDER_NAME_MAX_BYTES, FOLDER_NAME_MAX_CHARS,
    FOLDER_ORDER_MAX, FOLDER_PINS_MAX, INCLUDE_CHANNELS, INCLUDE_GROUPS, INCLUDE_ONE_TO_ONE,
    KNOWN_BITS, LINEAGE_GROUP_ID_MAX_LEN, SNAPSHOT_MAX_PARTS,
};
pub use lineage::{
    compose_lineage_envelope, decode_lineage_envelope, is_lineage_envelope, LineageEnvelopeError,
    LINEAGE_ENVELOPE_DOMAIN_V1, LINEAGE_ENVELOPE_MAX_ENTRIES, LINEAGE_ENVELOPE_MAX_LEN,
    LINEAGE_ENVELOPE_MIN_LEN, LINEAGE_ENVELOPE_VERSION_V1,
};
pub use state::{should_send_snapshot, ApplyOutcome, FolderView, SyncedState};

/// The largest encoded snapshot part a sender produces (ADR-0058 section 5:
/// 256 KiB; the DS accepts 1 MiB per publish).
pub const SNAPSHOT_MAX_PART_BYTES: usize = 256 * 1024;
