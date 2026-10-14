// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! State the account's own devices keep in step over the DeviceGroup
//! (ADR-0058): chat folders, the four older maps (flags, read cursors,
//! drafts, conversation order), and the account's saved messages (ADR-0065
//! section 4).
//!
//! ## Where the types live
//!
//! This crate owns plain Rust types and the fold; `lakko-mls` converts the
//! `devicegroup_v1.proto` messages into them when it decodes a DeviceGroup
//! payload, and back when it composes one. So the dependency runs
//! `lakko-mls -> lakko-sync`, never the other way, and this crate needs no
//! protobuf. The bounds of ADR-0058 section 4 and of the saved-message
//! register are written here once ([`entry::FolderChanges::validate`],
//! [`entry::StateSnapshot::validate`]) and the decoder calls them; the
//! display-name character rule comes from `lakko_contacts::profile`, the
//! one definition it has.
//!
//! ## Saved messages
//!
//! One register per saved message, keyed `(conversation, message id)`,
//! holding either the save (when, a display snapshot, the blob ids of the
//! message's files) or a removal tombstone, folded like every other
//! register. It rides in [`entry::FolderChanges::saved`], so a live Save or
//! Remove is an ordinary change set and a late device gets the list in the
//! snapshot. [`SAVED_MAX_CLIENT`] and [`SAVED_MAX_PER_CHANGE_SET`] are
//! client-side limits, not decode bounds.
//!
//! - [`entry`] — the register types, their bounds and their canonical
//!   encoding (the tie-break and the persistence element).
//! - [`lineage`] — the envelope the conversation-lineage register
//!   (ADR-0073 R.5) travels in between the account's devices.
//! - [`recovery_email`] — the envelope the account's recovery email register
//!   (ADR-0075) travels in between the account's devices.
//! - [`own_username`] — the envelope the account's own username register
//!   (ADR-0076) travels in between the account's devices.
//! - [`state`] — the fold ([`SyncedState`]), its queries, its persistence
//!   codec, and [`should_send_snapshot`].

pub mod entry;
pub mod lineage;
pub mod own_username;
pub mod recovery_email;
pub mod state;

pub use entry::{
    blob_id_from_slice, BlobId, SavedMessage, SavedSnapshot, SavedValue,
    SAVED_ATTACHMENT_KIND_MAX_BYTES, SAVED_AUTHOR_MAX_BYTES, SAVED_BLOB_IDS_MAX, SAVED_BLOB_ID_LEN,
    SAVED_EXCERPT_MAX_BYTES, SAVED_MAX_CLIENT, SAVED_MAX_PER_CHANGE_SET, SAVED_STATE_PRESENT,
    SAVED_STATE_REMOVED,
};
pub use entry::{
    check_folder_name, check_own_username, check_recovery_email_address, folder_id_from_slice,
    BoundsError, CodecError, ConversationEntry, ConversationOrderEntry, ConversationRef,
    ConversationRefKind, DraftEntry, FlagsEntry, FolderChanges, FolderId, FolderMember, FolderMeta,
    FolderOrder, FolderPins, LineageEntry, MemberState, ReadCursorEntry, RecoveryEmailEntry,
    RegisterKey, Stamped, StateSnapshot, SyncEntry, CONVERSATION_REF_ID_MAX_LEN, EXCLUDE_ARCHIVED,
    EXCLUDE_MUTED, EXCLUDE_READ, FOLDER_CHANGES_MAX_REGISTERS, FOLDER_ID_LEN,
    FOLDER_NAME_MAX_BYTES, FOLDER_NAME_MAX_CHARS, FOLDER_ORDER_MAX, FOLDER_PINS_MAX,
    INCLUDE_CHANNELS, INCLUDE_GROUPS, INCLUDE_ONE_TO_ONE, KNOWN_BITS, LINEAGE_GROUP_ID_MAX_LEN,
    RECOVERY_EMAIL_MAX_LEN, RECOVERY_EMAIL_MIN_LEN, SNAPSHOT_MAX_PARTS,
};
pub use entry::{OwnUsernameEntry, OWN_USERNAME_MAX_LEN};
pub use lineage::{
    compose_lineage_envelope, decode_lineage_envelope, is_lineage_envelope, LineageEnvelopeError,
    LINEAGE_ENVELOPE_DOMAIN_V1, LINEAGE_ENVELOPE_MAX_ENTRIES, LINEAGE_ENVELOPE_MAX_LEN,
    LINEAGE_ENVELOPE_MIN_LEN, LINEAGE_ENVELOPE_VERSION_V1,
};
pub use own_username::{
    compose_own_username_envelope, decode_own_username_envelope, is_own_username_envelope,
    OwnUsernameEnvelopeError, OWN_USERNAME_ENVELOPE_DOMAIN_V1, OWN_USERNAME_ENVELOPE_MAX_LEN,
    OWN_USERNAME_ENVELOPE_MIN_LEN, OWN_USERNAME_ENVELOPE_VERSION_V1,
};
pub use recovery_email::{
    compose_recovery_email_envelope, decode_recovery_email_envelope, is_recovery_email_envelope,
    RecoveryEmailEnvelopeError, RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1, RECOVERY_EMAIL_ENVELOPE_MAX_LEN,
    RECOVERY_EMAIL_ENVELOPE_MIN_LEN, RECOVERY_EMAIL_ENVELOPE_VERSION_V1,
};
pub use state::{should_send_snapshot, ApplyOutcome, FolderView, SavedView, SyncedState};

/// The largest encoded snapshot part a sender produces (ADR-0058 section 5:
/// 256 KiB; the DS accepts 1 MiB per publish).
pub const SNAPSHOT_MAX_PART_BYTES: usize = 256 * 1024;
