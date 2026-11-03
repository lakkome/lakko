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
//! ## Conversation ends
//!
//! One register per conversation an end has reached, saying the account's
//! membership ended or that it joined again (ADR-0087 section 9), with the MLS
//! epoch of that event. It rides in [`entry::FolderChanges::conversation_ends`]
//! as the saved register does. It is how a device that cannot apply the
//! commit, offline past the journal's retention, hears of an end.
//! [`CONVERSATION_ENDS_MAX_CLIENT`] is a client-side limit, not a decode
//! bound. See [`state`] for the order and for why a register is never dropped.
//!
//! - [`entry`] — the register types, their bounds and their canonical
//!   encoding (the tie-break and the persistence element).
//! - [`lineage`] — the envelope the conversation-lineage register
//!   (ADR-0073 R.5) travels in between the account's devices.
//! - [`recovery_email`] — the envelope the account's recovery email register
//!   (ADR-0075) travels in between the account's devices.
//! - [`link_record`] — the account's own invite links (ADR-0079), their
//!   verdict on a contact request's link answer, and their envelope.
//! - [`own_username`] — the envelope the account's own username register
//!   (ADR-0076) travels in between the account's devices.
//! - [`device_ack`] — the device-acknowledgement register (identity-recovery
//!   17.5: the added-device warning was seen) and its envelope.
//! - [`channel_reaction`] — the account's own reactions under channel posts
//!   (ADR-0087 section 13a), bounded to the posts the channel service still
//!   holds, and their envelope.
//! - [`channel_comment`] — the account's own comments under channel posts
//!   (the nonce each comment's write key is derived from, so any device can
//!   delete it), bounded the same way, and their envelope.
//! - [`state`] — the fold ([`SyncedState`]), its queries, its persistence
//!   codec, and [`should_send_snapshot`].

pub mod channel_comment;
pub mod channel_reaction;
pub mod device_ack;
pub mod entry;
pub mod lineage;
pub mod link_record;
pub mod own_username;
pub mod recovery_email;
pub mod state;

pub use channel_comment::{
    compose_channel_comment_envelope, decode_channel_comment_envelope, is_channel_comment_envelope,
    ChannelCommentEntry, ChannelCommentEnvelopeError, CHANNEL_COMMENTS_MAX_CLIENT,
    CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1, CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES,
    CHANNEL_COMMENT_ENVELOPE_MAX_LEN, CHANNEL_COMMENT_ENVELOPE_MIN_LEN,
    CHANNEL_COMMENT_ENVELOPE_VERSION_V1, CHANNEL_COMMENT_NONCE_LEN, CHANNEL_COMMENT_RETENTION_MS,
};
pub use channel_reaction::{
    compose_channel_reaction_envelope, decode_channel_reaction_envelope,
    is_channel_reaction_envelope, ChannelReactionEntry, ChannelReactionEnvelopeError,
    CHANNEL_REACTIONS_MAX_CLIENT, CHANNEL_REACTIONS_PER_POST_MAX, CHANNEL_REACTION_CHANNEL_ID_LEN,
    CHANNEL_REACTION_EMOJI_MAX_BYTES, CHANNEL_REACTION_ENVELOPE_DOMAIN_V1,
    CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES, CHANNEL_REACTION_ENVELOPE_MAX_LEN,
    CHANNEL_REACTION_ENVELOPE_MIN_LEN, CHANNEL_REACTION_ENVELOPE_VERSION_V1,
    CHANNEL_REACTION_RETENTION_MS,
};
pub use device_ack::{
    compose_device_ack_envelope, decode_device_ack_envelope, is_device_ack_envelope,
    DeviceAckEntry, DeviceAckEnvelopeError, DEVICE_ACK_ENVELOPE_DOMAIN_V1,
    DEVICE_ACK_ENVELOPE_MAX_ENTRIES, DEVICE_ACK_ENVELOPE_MAX_LEN, DEVICE_ACK_ENVELOPE_MIN_LEN,
    DEVICE_ACK_ENVELOPE_VERSION_V1, DEVICE_ACK_KEY_LEN,
};
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
pub use entry::{
    ConversationEnd, ConversationEndState, CONVERSATION_ENDED, CONVERSATION_ENDS_MAX_CLIENT,
    CONVERSATION_JOINED,
};
pub use entry::{OwnUsernameEntry, OWN_USERNAME_MAX_LEN};
pub use lineage::{
    compose_lineage_envelope, decode_lineage_envelope, is_lineage_envelope, LineageEnvelopeError,
    LINEAGE_ENVELOPE_DOMAIN_V1, LINEAGE_ENVELOPE_MAX_ENTRIES, LINEAGE_ENVELOPE_MAX_LEN,
    LINEAGE_ENVELOPE_MIN_LEN, LINEAGE_ENVELOPE_VERSION_V1,
};
pub use link_record::{
    compose_link_record_envelope, decode_link_record_envelope, is_link_record_envelope,
    LinkAnswerVerdict, LinkRecordEntry, LinkRecordEnvelopeError, LinkRecordKind,
    LINK_RECORD_ANSWERED_MAX, LINK_RECORD_ENVELOPE_DOMAIN_V1, LINK_RECORD_ENVELOPE_MAX_ENTRIES,
    LINK_RECORD_ENVELOPE_MAX_LEN, LINK_RECORD_ENVELOPE_MIN_LEN, LINK_RECORD_ENVELOPE_VERSION_V1,
    LINK_RECORD_ID_LEN, LINK_RECORD_LABEL_MAX_BYTES, LINK_RECORD_LEDGER_ID_LEN,
    LINK_RECORD_REQUESTER_KEY_LEN, LINK_RECORD_SECRET_LEN, LINK_RECORD_UNLIMITED,
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
