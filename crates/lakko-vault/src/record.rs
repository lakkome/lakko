// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The saved records (ADR-0064 section 2).
//!
//! The three clients keep their message stores in three shapes, and a
//! restored device may be a different platform from the one that saved. So
//! what goes into a vault object is one codec, defined here once: a
//! conversation's **head**, its **chunks** of earlier messages, the account's
//! **devices head**, and the **index** of the account's conversations.
//! Channel records are not part of this version.
//!
//! ## Wire
//!
//! All integers are big-endian. `opt<T>` is `0x00`, or `0x01` followed by
//! `T`; a `bool` is `0x00` or `0x01`; any other byte in either is refused.
//! Each of the four containers starts with `version(1) = 0x02` (version 1,
//! before ADR-0090, is refused).
//!
//! ```text
//! chunk := version || count u16 (<= 512) || message*
//!
//! message :=
//!   message_id u64 || direction u8 (0 outgoing, 1 incoming, 2 system)
//!   || opt<author(32)> || created_at_ms i64 || last_op_id u64
//!   || flags u8 (bit0 edited, bit1 deleted, others refused)
//!   || text_len u32 (<= 1 MiB) || text
//!   || attachment_count u8 (<= 64) || attachment*
//!   || opt<reply> || opt<forward>
//!   || reaction_count u16 (<= 1024) || reaction*
//!   || opt<call> || opt<unsupported_version u32> || opt<content>
//!
//! attachment :=
//!   blob_id(32) || key(32) || nonce(12) || size u64
//!   || mime_len u16 (<= 256) || mime || name_len u16 (<= 1024) || name
//!   || kind_len u8 (<= 32) || kind || opt<duration_ms u32>
//!   || waveform_len u8 (<= 128) || waveform
//!   || opt<width u16> || opt<height u16> || opt<thumb>
//!   || title_len u8 (<= 255) || title || artist_len u8 (<= 255) || artist
//! thumb :=
//!   blob_id(32) || key(32) || nonce(12) || mime_len u16 (<= 256) || mime
//!   || size u32 || opt<width u16> || opt<height u16>
//! reply := target u64 || author_len u16 (<= 255) || author
//!   || excerpt_len u16 (<= 512) || excerpt
//! forward := author_len u16 (<= 255) || author || opt<origin>
//! origin := group_id_len u8 (1..=128) || group_id || message_id u64
//! reaction := reactor(32) || emoji_len u8 (1..=64) || emoji
//! call := outcome u8 || video bool || duration_secs u32
//! content := kind u8 (1..=5) || payload_len u32 (1..=196608) || payload
//!   || state_len u16 (<= 8192) || state
//!
//! head := version || ref || kind u8 (0 unknown, 1 one-to-one, 2 group)
//!   || title_len u16 (<= 255) || title
//!   || people_count u16 (<= 4096) || person*
//!   || membership u8 (0 in, 1 left, 2 removed)
//!   || shared_pin_count u16 (<= 1024) || shared_pin*
//!   || personal_pin_count u16 (<= 1024) || personal_pin*
//!   || last_activity_ms u64
//!   || chunk_count u32 (<= 65536) || chunk_entry*
//!   || tail_count u16 (<= 512) || message*
//!   || reentry_count u8 (<= 2) || reentry*
//!   || opt<registrar>
//! ref := 0x01 || kind u8 (1 one-to-one, 2 group, 3 channel)
//!   || id_len u8 (1..=64) || id          (crate::keys::ConversationRef)
//! person := account_key(32) || display_name_len u8 || display_name
//!   || username_len u8 (<= 32) || username
//!   || role u8 (0 member, 1 admin, 2 owner)
//! shared_pin := message_id u64 || pinned bool || updated_at_ms u64
//! personal_pin := message_id u64 || pinned_at_ms u64
//! chunk_entry := count u16 || first_id u64 || last_id u64 || version u64
//! reentry := epoch u64 || group_info_len u32 (1..=1 MiB) || group_info
//! registrar := rank u8 || seed(32)
//!
//! devices_head := version || reentry_count u8 (<= 2) || reentry*
//!
//! index := version || entry_count u32 (<= 65536) || entry*
//! entry := ref || last_activity_ms u64 || membership u8
//! ```
//!
//! A content block (ADR-0090 section 3.11) is a poll (1), checklist (2),
//! location (3), contact card (4) or article (5): the JSON value of the
//! envelope's content field and, for a poll or checklist, a folded-state
//! snapshot (empty when none). Both are opaque here; the FFI decodes them
//! with `lakko_attachments::Content::from_payload` and
//! `lakko_attachments::content_state::ContentState::decode`.
//!
//! A string without a presence byte (`mime`, `name`, `kind`, `title`,
//! `artist`, a reply's or a
//! forward's `author`, `excerpt`, `title`, `display_name`, `username`) is
//! absent when it is empty; the same holds for `waveform`. Every string is
//! UTF-8.
//!
//! ## Canonical
//!
//! Decoding refuses trailing bytes, unknown versions, unknown flag bits,
//! enum and presence bytes outside their range, and every bound the encoder
//! checks. So one record has one encoding: whatever decodes re-encodes to the
//! identical bytes (fuzz targets `vault_head_decode`, `vault_chunk_decode`,
//! `vault_index_decode`).
//!
//! ## Hostile input
//!
//! These decoders run on what a vault object opens to. The object is
//! authenticated under a key only the account's devices hold, so the bytes
//! were written by one of them - possibly an older or a compromised one. The
//! decoders are written for arbitrary bytes all the same: the input is
//! refused above [`MAX_RECORD_BYTES`]; every length is checked against its
//! bound and then against the remaining input before it is used; every list
//! count is checked against its bound, and against the remaining input at
//! the smallest size an element can have, before anything is allocated; and
//! nothing panics. Errors are reason codes and never echo the input.
//!
//! ## What is not checked here
//!
//! The bounds are structural. The media rules of ADR-0047 (a duration only
//! for a voice or video note and within its limit, a dimension within its
//! range, a thumb within its size limit) live in `lakko-attachments`
//! (`RawMediaMetadata::bounded`), which this crate does not depend on; the
//! FFI crates apply them on both routes, as they do for the history bundle
//! of `lakko_mls::history_placeholder`, whose attachment field set and caps
//! this record mirrors. The display-name and username rules of ADR-0048 are
//! likewise applied when a profile is accepted; a head stores a snapshot of
//! what was shown.
//!
//! A record that is within every bound can still be too large for one
//! object: [`MAX_RECORD_BYTES`] is the plaintext an object carries
//! ([`crate::seal::MAX_PLAINTEXT_BYTES`]). The encoder refuses it and the
//! caller moves messages from the head's tail into chunks, or splits a
//! chunk.

use std::collections::BTreeSet;

use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::keys::{ConversationRef, MAX_CONVERSATION_ID_LEN, REF_TAG_CONVERSATION};
use crate::seal::MAX_PLAINTEXT_BYTES;

/// The one version byte the four containers carry. Version 2 (ADR-0090):
/// audio title and artist on an attachment, and the content block on a
/// message. Flag day: version 1 is refused.
pub const RECORD_VERSION_V2: u8 = 0x02;

/// Largest encoded record, in bytes: what one sealed object carries.
pub const MAX_RECORD_BYTES: usize = MAX_PLAINTEXT_BYTES;

/// Longest message text, in UTF-8 bytes (1 MiB).
pub const MAX_TEXT_BYTES: usize = 1 << 20;
/// Longest audio title or artist, bytes (ADR-0090).
pub const MAX_AUDIO_TAG_BYTES: usize = 255;
/// Largest content payload, bytes (chosen; matches
/// `lakko_attachments::content_state::MAX_CONTENT_PAYLOAD_BYTES`).
pub const MAX_CONTENT_PAYLOAD_BYTES: usize = 196_608;
/// Largest content state snapshot, bytes (chosen; matches
/// `lakko_attachments::content_state::MAX_CONTENT_STATE_BYTES`).
pub const MAX_CONTENT_STATE_BYTES: usize = 8_192;
/// Content kind bytes a record carries.
pub const CONTENT_KINDS: core::ops::RangeInclusive<u8> = 1..=5;
/// Most attachments on one message.
pub const MAX_ATTACHMENTS: usize = 64;
/// Longest attachment or thumb `mime`, in UTF-8 bytes.
pub const MAX_MIME_BYTES: usize = 256;
/// Longest attachment `name`, in UTF-8 bytes.
pub const MAX_NAME_BYTES: usize = 1024;
/// Longest attachment media `kind`, in UTF-8 bytes.
pub const MAX_KIND_BYTES: usize = 32;
/// Longest waveform, in bytes.
pub const MAX_WAVEFORM_BYTES: usize = 128;
/// Longest reply author snapshot, in UTF-8 bytes.
pub const MAX_REPLY_AUTHOR_BYTES: usize = 255;
/// Longest reply excerpt snapshot, in UTF-8 bytes.
pub const MAX_REPLY_EXCERPT_BYTES: usize = 512;
/// Longest forwarded-from author snapshot, in UTF-8 bytes.
pub const MAX_FORWARD_AUTHOR_BYTES: usize = 255;
/// Longest forward origin group reference, in bytes.
pub const MAX_FORWARD_ORIGIN_GROUP_BYTES: usize = 128;
/// Most reactions on one message.
pub const MAX_REACTIONS: usize = 1024;
/// Longest reaction emoji, in UTF-8 bytes.
pub const MAX_EMOJI_BYTES: usize = 64;
/// Most messages in one chunk.
pub const MAX_CHUNK_MESSAGES: usize = 512;
/// Longest conversation title, in UTF-8 bytes.
pub const MAX_TITLE_BYTES: usize = 255;
/// Most people in one head.
pub const MAX_PEOPLE: usize = 4096;
/// Longest display-name snapshot, in UTF-8 bytes.
pub const MAX_DISPLAY_NAME_BYTES: usize = 255;
/// Longest username snapshot, in UTF-8 bytes.
pub const MAX_USERNAME_BYTES: usize = 32;
/// Most shared pins, and most personal pins, in one head.
pub const MAX_PINS: usize = 1024;
/// Most entries in a head's chunk table.
pub const MAX_CHUNKS: usize = 65_536;
/// Most messages in a head's tail.
pub const MAX_TAIL_MESSAGES: usize = 512;
/// Most saved epochs of re-entry material: the two newest.
pub const MAX_REENTRY_EPOCHS: usize = 2;
/// Longest saved GroupInfo, in bytes (1 MiB).
pub const MAX_GROUP_INFO_BYTES: usize = 1 << 20;
/// Most entries in the index.
pub const MAX_INDEX_ENTRIES: usize = 65_536;

const FLAG_EDITED: u8 = 1 << 0;
const FLAG_DELETED: u8 = 1 << 1;
const FLAG_KNOWN: u8 = FLAG_EDITED | FLAG_DELETED;

// The smallest encoding of one list element, with every variable-length
// field at length zero - also those whose own rule requires a byte, so that
// an empty one is refused by that rule and not as a short input. A count is
// checked against the remaining input at this size before a list is
// allocated.
const MIN_MESSAGE_LEN: usize = 8 + 1 + 1 + 8 + 8 + 1 + 4 + 1 + 1 + 1 + 2 + 1 + 1 + 1;
const MIN_ATTACHMENT_LEN: usize = 32 + 32 + 12 + 8 + 2 + 2 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1;
const MIN_REACTION_LEN: usize = 32 + 1;
const MIN_PERSON_LEN: usize = 32 + 1 + 1 + 1;
const SHARED_PIN_LEN: usize = 8 + 1 + 8;
const PERSONAL_PIN_LEN: usize = 8 + 8;
const CHUNK_ENTRY_LEN: usize = 2 + 8 + 8 + 8;
const MIN_REENTRY_LEN: usize = 8 + 4;
const MIN_INDEX_ENTRY_LEN: usize = 3 + 8 + 1;

/// Why a record was refused. Reason codes only: no variant carries input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RecordError {
    /// Longer than [`MAX_RECORD_BYTES`].
    #[error("vault record is too large")]
    TooLarge,
    /// A version this codec does not read.
    #[error("unknown vault record version")]
    UnknownVersion,
    /// The input ended before a field did.
    #[error("vault record is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("vault record has trailing bytes")]
    TrailingBytes,
    /// A flag bit outside the defined set is set.
    #[error("vault record has an unknown flag")]
    UnknownFlag,
    /// An enum, presence or boolean byte outside its range.
    #[error("vault record has an unknown option")]
    UnknownOption,
    /// A string field is not UTF-8.
    #[error("vault record has text that is not UTF-8")]
    NotUtf8,
    /// A conversation reference is not admissible.
    #[error("the conversation reference is not admissible")]
    Ref,
    /// A message text over [`MAX_TEXT_BYTES`].
    #[error("a message text is too long")]
    Text,
    /// More than [`MAX_ATTACHMENTS`] attachments.
    #[error("a message has too many attachments")]
    Attachments,
    /// An attachment or thumb field over its bound.
    #[error("an attachment field is too long")]
    Attachment,
    /// A reply snapshot over its bound.
    #[error("a reply snapshot is too long")]
    Reply,
    /// A forward author over its bound, or an origin group reference that is
    /// empty or too long.
    #[error("a forward snapshot is not admissible")]
    Forward,
    /// Too many reactions, or an emoji that is empty or too long.
    #[error("the reaction list is not admissible")]
    Reactions,
    /// Too many messages in a chunk or in a head's tail.
    #[error("the message list is too long")]
    Messages,
    /// A title over [`MAX_TITLE_BYTES`].
    #[error("the title is too long")]
    Title,
    /// Too many people, or a name snapshot over its bound.
    #[error("the people list is not admissible")]
    People,
    /// Too many pins.
    #[error("the pin list is too long")]
    Pins,
    /// Too many chunk table entries.
    #[error("the chunk table is too long")]
    Chunks,
    /// Too many saved epochs, or a GroupInfo that is empty or too long.
    #[error("the re-entry material is not admissible")]
    Reentry,
    /// Too many index entries.
    #[error("the index is too long")]
    Entries,
    /// The index names one conversation twice.
    #[error("the index repeats a conversation")]
    RepeatedRef,
    /// A content block with an unknown kind, an empty or over-long payload,
    /// or an over-long state (ADR-0090).
    #[error("a content block is not admissible")]
    Content,
}

impl RecordError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "tooLarge",
            Self::UnknownVersion => "unknownVersion",
            Self::Truncated => "truncated",
            Self::TrailingBytes => "trailingBytes",
            Self::UnknownFlag => "unknownFlag",
            Self::UnknownOption => "unknownOption",
            Self::NotUtf8 => "notUtf8",
            Self::Ref => "ref",
            Self::Text => "text",
            Self::Attachments => "attachments",
            Self::Attachment => "attachment",
            Self::Reply => "reply",
            Self::Forward => "forward",
            Self::Reactions => "reactions",
            Self::Messages => "messages",
            Self::Title => "title",
            Self::People => "people",
            Self::Pins => "pins",
            Self::Chunks => "chunks",
            Self::Reentry => "reentry",
            Self::Entries => "entries",
            Self::RepeatedRef => "repeatedRef",
            Self::Content => "content",
        }
    }
}

/// Declare an enum that travels as one byte, with its two conversions.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $(#[$vmeta:meta])* $variant:ident = $value:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// The wire byte.
            #[must_use]
            pub const fn to_wire(self) -> u8 {
                match self {
                    $( Self::$variant => $value, )+
                }
            }

            /// Read a wire byte; anything unknown is `None`.
            #[must_use]
            pub const fn from_wire(byte: u8) -> Option<Self> {
                match byte {
                    $( $value => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }
    };
}

wire_enum! {
    /// A message's direction relative to the account.
    Direction {
        /// Sent by this account.
        Outgoing = 0,
        /// Received from someone else.
        Incoming = 1,
        /// A system row (a membership change, a call).
        System = 2,
    }
}

wire_enum! {
    /// What kind of conversation a head describes (ADR-0062).
    ConversationKind {
        /// The saving device did not know.
        Unknown = 0,
        /// A one-to-one conversation.
        OneToOne = 1,
        /// A group.
        Group = 2,
    }
}

wire_enum! {
    /// A person's role at save time (ADR-0022).
    Role {
        /// A plain member.
        Member = 0,
        /// An admin.
        Admin = 1,
        /// The owner.
        Owner = 2,
    }
}

wire_enum! {
    /// The account's membership at save time.
    Membership {
        /// In the conversation.
        In = 0,
        /// Left it.
        Left = 1,
        /// Was removed from it.
        Removed = 2,
    }
}

/// A poster-frame blob of an attachment.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultThumb {
    /// 32-byte blob id.
    pub blob_id: [u8; 32],
    /// 32-byte content-encryption key.
    pub key: [u8; 32],
    /// 12-byte AEAD nonce.
    pub nonce: [u8; 12],
    /// MIME type; empty when none.
    pub mime: String,
    /// Plaintext size in bytes.
    pub size: u32,
    /// Pixel width.
    pub width: Option<u16>,
    /// Pixel height.
    pub height: Option<u16>,
}

impl core::fmt::Debug for VaultThumb {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VaultThumb")
            .field("blob_id", &self.blob_id)
            .field("key", &"<redacted>")
            .field("nonce", &self.nonce)
            .field("mime", &self.mime)
            .field("size", &self.size)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// An attachment reference: where the encrypted file is and how to open it.
/// A reference, not the file, which may have expired by the time the history
/// is read (ADR-0065 section 8).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultAttachment {
    /// 32-byte blob id.
    pub blob_id: [u8; 32],
    /// 32-byte content-encryption key.
    pub key: [u8; 32],
    /// 12-byte AEAD nonce.
    pub nonce: [u8; 12],
    /// Plaintext size in bytes.
    pub size: u64,
    /// MIME type; empty when none.
    pub mime: String,
    /// File name; empty when none.
    pub name: String,
    /// ADR-0047 media kind (`voice`, `video_note`, `gif`, or one this build
    /// does not know); empty for a plain file.
    pub kind: String,
    /// Playback length in milliseconds.
    pub duration_ms: Option<u32>,
    /// Waveform amplitudes; empty when none.
    pub waveform: Vec<u8>,
    /// Pixel width.
    pub width: Option<u16>,
    /// Pixel height.
    pub height: Option<u16>,
    /// Poster frame.
    pub thumb: Option<VaultThumb>,
    /// ADR-0090 audio title; empty when none.
    pub title: String,
    /// ADR-0090 audio artist; empty when none.
    pub artist: String,
}

impl core::fmt::Debug for VaultAttachment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VaultAttachment")
            .field("blob_id", &self.blob_id)
            .field("key", &"<redacted>")
            .field("nonce", &self.nonce)
            .field("size", &self.size)
            .field("mime", &self.mime)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("duration_ms", &self.duration_ms)
            .field("waveform", &self.waveform)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("thumb", &self.thumb)
            .field("title", &self.title)
            .field("artist", &self.artist)
            .finish()
    }
}

/// The quote a reply carries (ADR-0052): a snapshot taken when the reply was
/// composed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultReply {
    /// Message id of the quoted message.
    pub target: u64,
    /// The quoted author's display name; empty when none.
    pub author: String,
    /// An excerpt of the quoted text; empty when none.
    pub excerpt: String,
}

/// Where a forwarded message came from (ADR-0052).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultForwardOrigin {
    /// The origin conversation, as the message carried it. Opaque here.
    pub group_id: Vec<u8>,
    /// Message id of the original in that conversation.
    pub message_id: u64,
}

/// What a forwarded message says about its origin (ADR-0052).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultForward {
    /// The original author's display name; empty when none.
    pub author: String,
    /// The original's location, when the forwarder held it.
    pub origin: Option<VaultForwardOrigin>,
}

/// One reaction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultReaction {
    /// The reacting person's account key.
    pub reactor: [u8; 32],
    /// The emoji.
    pub emoji: String,
}

/// How a call row ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultCall {
    /// The outcome, as the clients number it. Opaque here.
    pub outcome: u8,
    /// Whether it was a video call.
    pub video: bool,
    /// How long it lasted, in seconds.
    pub duration_secs: u32,
}

/// An ADR-0090 content message: kind, payload and state snapshot, opaque
/// here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultContent {
    /// Content kind byte, in [`CONTENT_KINDS`].
    pub kind: u8,
    /// The payload, 1..=[`MAX_CONTENT_PAYLOAD_BYTES`] bytes.
    pub payload: Vec<u8>,
    /// The folded-state snapshot; empty when none.
    pub state: Vec<u8>,
}

/// One saved message: the row shape of a head's tail and of every chunk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VaultMessage {
    /// The Delivery Service journal id, which every device observes
    /// identically. The merge key.
    pub message_id: u64,
    /// Direction relative to the account.
    pub direction: Direction,
    /// The author's account key, when known.
    pub author: Option<[u8; 32]>,
    /// When it was sent, unix milliseconds.
    pub created_at_ms: i64,
    /// Journal id of the newest edit, delete or reaction applied to this
    /// row; the row's own id when none was. Decides a merge.
    pub last_op_id: u64,
    /// The text was edited.
    pub edited: bool,
    /// The message was deleted; the row is kept, emptied.
    pub deleted: bool,
    /// The text.
    pub text: String,
    /// Attachment references.
    pub attachments: Vec<VaultAttachment>,
    /// The quote, for a reply.
    pub reply: Option<VaultReply>,
    /// The origin, for a forwarded message.
    pub forward: Option<VaultForward>,
    /// Reactions, keyed by account key.
    pub reactions: Vec<VaultReaction>,
    /// The outcome, for a call row.
    pub call: Option<VaultCall>,
    /// Set when the message was of a wire version the saving device could
    /// not read: that version.
    pub unsupported_version: Option<u32>,
    /// ADR-0090 content, for a poll, checklist, location, contact card or
    /// article message.
    pub content: Option<VaultContent>,
}

/// One chunk of earlier messages.
///
/// The codec asks for no order and does not refuse a repeated id: a chunk is
/// whatever a device saved. [`crate::merge::merge_messages`] is what yields
/// rows sorted by id, each id once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryChunk {
    /// The messages, at most [`MAX_CHUNK_MESSAGES`].
    pub messages: Vec<VaultMessage>,
}

/// One person in a conversation, as shown at save time. Never a device leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPerson {
    /// The person's account key.
    pub account_key: [u8; 32],
    /// Display-name snapshot; empty when none.
    pub display_name: String,
    /// Username snapshot; empty when none.
    pub username: String,
    /// Role at save time.
    pub role: Role,
}

/// A pin shared with the conversation (ADR-0052). An unpin is kept, so a
/// stale pin cannot bring it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedPin {
    /// The pinned message.
    pub message_id: u64,
    /// Pinned, or unpinned.
    pub pinned: bool,
    /// The stamp of this state, unix milliseconds.
    pub updated_at_ms: u64,
}

/// A pin of this account alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonalPin {
    /// The pinned message.
    pub message_id: u64,
    /// When it was pinned, unix milliseconds.
    pub pinned_at_ms: u64,
}

/// One row of a head's chunk table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkEntry {
    /// How many messages the chunk holds.
    pub count: u16,
    /// Its lowest message id.
    pub first_id: u64,
    /// Its highest message id.
    pub last_id: u64,
    /// The chunk object's current version on the storage service.
    pub version: u64,
}

/// Re-entry material for one epoch (ADR-0064 section 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReentryEpoch {
    /// The epoch the GroupInfo describes.
    pub epoch: u64,
    /// The GroupInfo with its ratchet tree, as MLS encodes it. Opaque here.
    pub group_info: Vec<u8>,
}

/// The account's registrar seed for a conversation's journal (ADR-0057).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrarSeed {
    /// The rank the seed was issued at. Opaque here.
    pub rank: u8,
    /// The seed.
    pub seed: SecretBytes<32>,
}

/// A conversation's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHead {
    /// Which conversation.
    pub conversation: ConversationRef,
    /// One-to-one or group.
    pub kind: ConversationKind,
    /// The group name; empty for a one-to-one or an unnamed group.
    pub title: String,
    /// The people, at most [`MAX_PEOPLE`].
    pub people: Vec<VaultPerson>,
    /// The account's membership at save time.
    pub membership: Membership,
    /// Shared pins, at most [`MAX_PINS`].
    pub shared_pins: Vec<SharedPin>,
    /// Personal pins, at most [`MAX_PINS`].
    pub personal_pins: Vec<PersonalPin>,
    /// Last activity, unix milliseconds.
    pub last_activity_ms: u64,
    /// The chunk table, at most [`MAX_CHUNKS`].
    pub chunks: Vec<ChunkEntry>,
    /// The newest messages, not yet moved into a chunk; at most
    /// [`MAX_TAIL_MESSAGES`].
    pub tail: Vec<VaultMessage>,
    /// Re-entry material for the newest epochs, at most
    /// [`MAX_REENTRY_EPOCHS`]; empty when the account is not in the
    /// conversation.
    pub reentry: Vec<ReentryEpoch>,
    /// The account's registrar seed for this journal, when it holds one.
    pub registrar_seed: Option<RegistrarSeed>,
}

/// The head of the account's own device group (ADR-0064 section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicesHead {
    /// Re-entry material for the newest epochs, at most
    /// [`MAX_REENTRY_EPOCHS`].
    pub reentry: Vec<ReentryEpoch>,
}

/// One conversation in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Which conversation.
    pub conversation: ConversationRef,
    /// Last activity, unix milliseconds. Orders the loading.
    pub last_activity_ms: u64,
    /// The account's membership at save time.
    pub membership: Membership,
}

/// The account's conversations (ADR-0064 section 1): what a device reads
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultIndex {
    /// The entries, at most [`MAX_INDEX_ENTRIES`], no conversation twice.
    pub entries: Vec<IndexEntry>,
}

// --- validation -------------------------------------------------------

const fn within(len: usize, max: usize, err: RecordError) -> Result<(), RecordError> {
    if len > max {
        Err(err)
    } else {
        Ok(())
    }
}

impl VaultThumb {
    fn validate(&self) -> Result<(), RecordError> {
        within(self.mime.len(), MAX_MIME_BYTES, RecordError::Attachment)
    }
}

impl VaultAttachment {
    fn validate(&self) -> Result<(), RecordError> {
        within(self.mime.len(), MAX_MIME_BYTES, RecordError::Attachment)?;
        within(self.name.len(), MAX_NAME_BYTES, RecordError::Attachment)?;
        within(self.kind.len(), MAX_KIND_BYTES, RecordError::Attachment)?;
        within(
            self.waveform.len(),
            MAX_WAVEFORM_BYTES,
            RecordError::Attachment,
        )?;
        within(
            self.title.len(),
            MAX_AUDIO_TAG_BYTES,
            RecordError::Attachment,
        )?;
        within(
            self.artist.len(),
            MAX_AUDIO_TAG_BYTES,
            RecordError::Attachment,
        )?;
        self.thumb.as_ref().map_or(Ok(()), VaultThumb::validate)
    }
}

impl VaultMessage {
    /// Check every bound of one message. The single rule the encoders and
    /// the decoders run.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first field that is out of bounds.
    pub fn validate(&self) -> Result<(), RecordError> {
        within(self.text.len(), MAX_TEXT_BYTES, RecordError::Text)?;
        within(
            self.attachments.len(),
            MAX_ATTACHMENTS,
            RecordError::Attachments,
        )?;
        self.attachments
            .iter()
            .try_for_each(VaultAttachment::validate)?;
        if let Some(reply) = &self.reply {
            within(
                reply.author.len(),
                MAX_REPLY_AUTHOR_BYTES,
                RecordError::Reply,
            )?;
            within(
                reply.excerpt.len(),
                MAX_REPLY_EXCERPT_BYTES,
                RecordError::Reply,
            )?;
        }
        if let Some(forward) = &self.forward {
            within(
                forward.author.len(),
                MAX_FORWARD_AUTHOR_BYTES,
                RecordError::Forward,
            )?;
            if let Some(origin) = &forward.origin {
                if origin.group_id.is_empty() {
                    return Err(RecordError::Forward);
                }
                within(
                    origin.group_id.len(),
                    MAX_FORWARD_ORIGIN_GROUP_BYTES,
                    RecordError::Forward,
                )?;
            }
        }
        within(self.reactions.len(), MAX_REACTIONS, RecordError::Reactions)?;
        for reaction in &self.reactions {
            if reaction.emoji.is_empty() {
                return Err(RecordError::Reactions);
            }
            within(
                reaction.emoji.len(),
                MAX_EMOJI_BYTES,
                RecordError::Reactions,
            )?;
        }
        if let Some(content) = &self.content {
            if !CONTENT_KINDS.contains(&content.kind) || content.payload.is_empty() {
                return Err(RecordError::Content);
            }
            within(
                content.payload.len(),
                MAX_CONTENT_PAYLOAD_BYTES,
                RecordError::Content,
            )?;
            within(
                content.state.len(),
                MAX_CONTENT_STATE_BYTES,
                RecordError::Content,
            )?;
        }
        Ok(())
    }

    const fn flags(&self) -> u8 {
        let mut flags = 0;
        if self.edited {
            flags |= FLAG_EDITED;
        }
        if self.deleted {
            flags |= FLAG_DELETED;
        }
        flags
    }

    /// The message's bytes as a chunk or a tail carries them.
    ///
    /// For a message within its bounds this is the canonical encoding.
    /// Outside them a length prefix saturates, which no decoder accepts; the
    /// merge still gets a deterministic byte string to compare.
    pub(crate) fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_message(&mut out, self);
        out
    }
}

fn validate_messages(messages: &[VaultMessage], max: usize) -> Result<(), RecordError> {
    within(messages.len(), max, RecordError::Messages)?;
    messages.iter().try_for_each(VaultMessage::validate)
}

fn validate_reentry(reentry: &[ReentryEpoch]) -> Result<(), RecordError> {
    within(reentry.len(), MAX_REENTRY_EPOCHS, RecordError::Reentry)?;
    for epoch in reentry {
        if epoch.group_info.is_empty() {
            return Err(RecordError::Reentry);
        }
        within(
            epoch.group_info.len(),
            MAX_GROUP_INFO_BYTES,
            RecordError::Reentry,
        )?;
    }
    Ok(())
}

impl HistoryChunk {
    /// Check every bound except the encoded size.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first field that is out of bounds.
    pub fn validate(&self) -> Result<(), RecordError> {
        validate_messages(&self.messages, MAX_CHUNK_MESSAGES)
    }
}

impl ConversationHead {
    /// Check every bound except the encoded size.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first field that is out of bounds.
    pub fn validate(&self) -> Result<(), RecordError> {
        within(self.title.len(), MAX_TITLE_BYTES, RecordError::Title)?;
        within(self.people.len(), MAX_PEOPLE, RecordError::People)?;
        for person in &self.people {
            within(
                person.display_name.len(),
                MAX_DISPLAY_NAME_BYTES,
                RecordError::People,
            )?;
            within(
                person.username.len(),
                MAX_USERNAME_BYTES,
                RecordError::People,
            )?;
        }
        within(self.shared_pins.len(), MAX_PINS, RecordError::Pins)?;
        within(self.personal_pins.len(), MAX_PINS, RecordError::Pins)?;
        within(self.chunks.len(), MAX_CHUNKS, RecordError::Chunks)?;
        validate_messages(&self.tail, MAX_TAIL_MESSAGES)?;
        validate_reentry(&self.reentry)
    }
}

impl DevicesHead {
    /// Check every bound except the encoded size.
    ///
    /// # Errors
    ///
    /// [`RecordError::Reentry`].
    pub fn validate(&self) -> Result<(), RecordError> {
        validate_reentry(&self.reentry)
    }
}

impl VaultIndex {
    /// Check every bound except the encoded size.
    ///
    /// # Errors
    ///
    /// [`RecordError::Entries`] or [`RecordError::RepeatedRef`].
    pub fn validate(&self) -> Result<(), RecordError> {
        within(self.entries.len(), MAX_INDEX_ENTRIES, RecordError::Entries)?;
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            if !seen.insert(&entry.conversation) {
                return Err(RecordError::RepeatedRef);
            }
        }
        Ok(())
    }
}

// --- encoding ---------------------------------------------------------
//
// The writers are infallible: `validate` has bounded every length before a
// container is written, so each fits its prefix. The saturating conversions
// are there so that nothing can panic, not because a valid record reaches
// them.

fn put_len_u8(out: &mut Vec<u8>, len: usize) {
    out.push(u8::try_from(len).unwrap_or(u8::MAX));
}

fn put_len_u16(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes());
}

fn put_len_u32(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_bytes_u8(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len_u8(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn put_bytes_u16(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len_u16(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn put_bytes_u32(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len_u32(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn put_bool(out: &mut Vec<u8>, value: bool) {
    out.push(u8::from(value));
}

fn put_opt_u16(out: &mut Vec<u8>, value: Option<u16>) {
    put_bool(out, value.is_some());
    if let Some(v) = value {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn put_opt_u32(out: &mut Vec<u8>, value: Option<u32>) {
    put_bool(out, value.is_some());
    if let Some(v) = value {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn put_thumb(out: &mut Vec<u8>, thumb: &VaultThumb) {
    out.extend_from_slice(&thumb.blob_id);
    out.extend_from_slice(&thumb.key);
    out.extend_from_slice(&thumb.nonce);
    put_bytes_u16(out, thumb.mime.as_bytes());
    out.extend_from_slice(&thumb.size.to_be_bytes());
    put_opt_u16(out, thumb.width);
    put_opt_u16(out, thumb.height);
}

fn put_attachment(out: &mut Vec<u8>, attachment: &VaultAttachment) {
    out.extend_from_slice(&attachment.blob_id);
    out.extend_from_slice(&attachment.key);
    out.extend_from_slice(&attachment.nonce);
    out.extend_from_slice(&attachment.size.to_be_bytes());
    put_bytes_u16(out, attachment.mime.as_bytes());
    put_bytes_u16(out, attachment.name.as_bytes());
    put_bytes_u8(out, attachment.kind.as_bytes());
    put_opt_u32(out, attachment.duration_ms);
    put_bytes_u8(out, &attachment.waveform);
    put_opt_u16(out, attachment.width);
    put_opt_u16(out, attachment.height);
    put_bool(out, attachment.thumb.is_some());
    if let Some(thumb) = &attachment.thumb {
        put_thumb(out, thumb);
    }
    put_bytes_u8(out, attachment.title.as_bytes());
    put_bytes_u8(out, attachment.artist.as_bytes());
}

fn put_message(out: &mut Vec<u8>, message: &VaultMessage) {
    out.extend_from_slice(&message.message_id.to_be_bytes());
    out.push(message.direction.to_wire());
    put_bool(out, message.author.is_some());
    if let Some(author) = &message.author {
        out.extend_from_slice(author);
    }
    out.extend_from_slice(&message.created_at_ms.to_be_bytes());
    out.extend_from_slice(&message.last_op_id.to_be_bytes());
    out.push(message.flags());
    put_bytes_u32(out, message.text.as_bytes());
    put_len_u8(out, message.attachments.len());
    for attachment in &message.attachments {
        put_attachment(out, attachment);
    }
    put_bool(out, message.reply.is_some());
    if let Some(reply) = &message.reply {
        out.extend_from_slice(&reply.target.to_be_bytes());
        put_bytes_u16(out, reply.author.as_bytes());
        put_bytes_u16(out, reply.excerpt.as_bytes());
    }
    put_bool(out, message.forward.is_some());
    if let Some(forward) = &message.forward {
        put_bytes_u16(out, forward.author.as_bytes());
        put_bool(out, forward.origin.is_some());
        if let Some(origin) = &forward.origin {
            put_bytes_u8(out, &origin.group_id);
            out.extend_from_slice(&origin.message_id.to_be_bytes());
        }
    }
    put_len_u16(out, message.reactions.len());
    for reaction in &message.reactions {
        out.extend_from_slice(&reaction.reactor);
        put_bytes_u8(out, reaction.emoji.as_bytes());
    }
    put_bool(out, message.call.is_some());
    if let Some(call) = &message.call {
        out.push(call.outcome);
        put_bool(out, call.video);
        out.extend_from_slice(&call.duration_secs.to_be_bytes());
    }
    put_opt_u32(out, message.unsupported_version);
    put_bool(out, message.content.is_some());
    if let Some(content) = &message.content {
        out.push(content.kind);
        put_bytes_u32(out, &content.payload);
        put_bytes_u16(out, &content.state);
    }
}

const fn check_size(out: &[u8]) -> Result<(), RecordError> {
    within(out.len(), MAX_RECORD_BYTES, RecordError::TooLarge)
}

/// Write a message list after its count. The size is checked after every
/// message so an over-large record is refused without being built whole.
fn put_messages(out: &mut Vec<u8>, messages: &[VaultMessage]) -> Result<(), RecordError> {
    put_len_u16(out, messages.len());
    for message in messages {
        put_message(out, message);
        check_size(out)?;
    }
    Ok(())
}

fn put_reentry(out: &mut Vec<u8>, reentry: &[ReentryEpoch]) -> Result<(), RecordError> {
    put_len_u8(out, reentry.len());
    for epoch in reentry {
        out.extend_from_slice(&epoch.epoch.to_be_bytes());
        put_bytes_u32(out, &epoch.group_info);
        check_size(out)?;
    }
    Ok(())
}

impl HistoryChunk {
    /// Encode the chunk.
    ///
    /// # Errors
    ///
    /// A bound from [`Self::validate`], or [`RecordError::TooLarge`] when
    /// the encoding exceeds [`MAX_RECORD_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        self.write()
    }

    /// The layout, without the bounds: [`Self::encode`] has checked them.
    /// Separate so the tests can hand the decoder an encoding that is one
    /// past a bound.
    fn write(&self) -> Result<Vec<u8>, RecordError> {
        let mut out = vec![RECORD_VERSION_V2];
        put_messages(&mut out, &self.messages)?;
        check_size(&out)?;
        Ok(out)
    }
}

impl ConversationHead {
    /// Encode the head.
    ///
    /// # Errors
    ///
    /// A bound from [`Self::validate`], or [`RecordError::TooLarge`] when
    /// the encoding exceeds [`MAX_RECORD_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        self.write()
    }

    /// The layout, without the bounds: [`Self::encode`] has checked them.
    /// Separate so the tests can hand the decoder an encoding that is one
    /// past a bound.
    fn write(&self) -> Result<Vec<u8>, RecordError> {
        let mut out = vec![RECORD_VERSION_V2];
        self.conversation.write_to(&mut out);
        out.push(self.kind.to_wire());
        put_bytes_u16(&mut out, self.title.as_bytes());
        put_len_u16(&mut out, self.people.len());
        for person in &self.people {
            out.extend_from_slice(&person.account_key);
            put_bytes_u8(&mut out, person.display_name.as_bytes());
            put_bytes_u8(&mut out, person.username.as_bytes());
            out.push(person.role.to_wire());
        }
        out.push(self.membership.to_wire());
        put_len_u16(&mut out, self.shared_pins.len());
        for pin in &self.shared_pins {
            out.extend_from_slice(&pin.message_id.to_be_bytes());
            put_bool(&mut out, pin.pinned);
            out.extend_from_slice(&pin.updated_at_ms.to_be_bytes());
        }
        put_len_u16(&mut out, self.personal_pins.len());
        for pin in &self.personal_pins {
            out.extend_from_slice(&pin.message_id.to_be_bytes());
            out.extend_from_slice(&pin.pinned_at_ms.to_be_bytes());
        }
        out.extend_from_slice(&self.last_activity_ms.to_be_bytes());
        put_len_u32(&mut out, self.chunks.len());
        for chunk in &self.chunks {
            out.extend_from_slice(&chunk.count.to_be_bytes());
            out.extend_from_slice(&chunk.first_id.to_be_bytes());
            out.extend_from_slice(&chunk.last_id.to_be_bytes());
            out.extend_from_slice(&chunk.version.to_be_bytes());
        }
        check_size(&out)?;
        put_messages(&mut out, &self.tail)?;
        put_reentry(&mut out, &self.reentry)?;
        put_bool(&mut out, self.registrar_seed.is_some());
        if let Some(registrar) = &self.registrar_seed {
            out.push(registrar.rank);
            out.extend_from_slice(registrar.seed.as_bytes());
        }
        check_size(&out)?;
        Ok(out)
    }
}

impl DevicesHead {
    /// Encode the devices head.
    ///
    /// # Errors
    ///
    /// [`RecordError::Reentry`], or [`RecordError::TooLarge`] when the
    /// encoding exceeds [`MAX_RECORD_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        self.write()
    }

    /// The layout, without the bounds: [`Self::encode`] has checked them.
    /// Separate so the tests can hand the decoder an encoding that is one
    /// past a bound.
    fn write(&self) -> Result<Vec<u8>, RecordError> {
        let mut out = vec![RECORD_VERSION_V2];
        put_reentry(&mut out, &self.reentry)?;
        check_size(&out)?;
        Ok(out)
    }
}

impl VaultIndex {
    /// Encode the index.
    ///
    /// # Errors
    ///
    /// A bound from [`Self::validate`], or [`RecordError::TooLarge`] when
    /// the encoding exceeds [`MAX_RECORD_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        self.write()
    }

    /// The layout, without the bounds: [`Self::encode`] has checked them.
    /// Separate so the tests can hand the decoder an encoding that is one
    /// past a bound.
    fn write(&self) -> Result<Vec<u8>, RecordError> {
        let mut out = vec![RECORD_VERSION_V2];
        put_len_u32(&mut out, self.entries.len());
        for entry in &self.entries {
            entry.conversation.write_to(&mut out);
            out.extend_from_slice(&entry.last_activity_ms.to_be_bytes());
            out.push(entry.membership.to_wire());
        }
        check_size(&out)?;
        Ok(out)
    }
}

// --- decoding ---------------------------------------------------------

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Open a container: the size bound, then the version byte.
    fn open(bytes: &'a [u8]) -> Result<Self, RecordError> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(RecordError::TooLarge);
        }
        let mut reader = Self { rest: bytes };
        if reader.u8()? != RECORD_VERSION_V2 {
            return Err(RecordError::UnknownVersion);
        }
        Ok(reader)
    }

    const fn finish(&self) -> Result<(), RecordError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(RecordError::TrailingBytes)
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RecordError> {
        if self.rest.len() < n {
            return Err(RecordError::Truncated);
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], RecordError> {
        let Some((head, tail)) = self.rest.split_first_chunk::<N>() else {
            return Err(RecordError::Truncated);
        };
        self.rest = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, RecordError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    fn u16(&mut self) -> Result<u16, RecordError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, RecordError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, RecordError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, RecordError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    /// A boolean or a presence byte: `0x00` or `0x01`, nothing else.
    fn bool(&mut self) -> Result<bool, RecordError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(RecordError::UnknownOption),
        }
    }

    fn opt_u16(&mut self) -> Result<Option<u16>, RecordError> {
        if self.bool()? {
            Ok(Some(self.u16()?))
        } else {
            Ok(None)
        }
    }

    fn opt_u32(&mut self) -> Result<Option<u32>, RecordError> {
        if self.bool()? {
            Ok(Some(self.u32()?))
        } else {
            Ok(None)
        }
    }

    fn len_u8(&mut self) -> Result<usize, RecordError> {
        Ok(usize::from(self.u8()?))
    }

    fn len_u16(&mut self) -> Result<usize, RecordError> {
        Ok(usize::from(self.u16()?))
    }

    /// A 32-bit length. A value that does not fit `usize` reads as the
    /// largest one, which every bound refuses.
    fn len_u32(&mut self) -> Result<usize, RecordError> {
        Ok(usize::try_from(self.u32()?).unwrap_or(usize::MAX))
    }

    /// `len` bytes, after checking `len` against `max`. The bound comes
    /// first, so an over-long field is named as such even when the input is
    /// also short.
    fn bounded(
        &mut self,
        len: usize,
        max: usize,
        err: RecordError,
    ) -> Result<&'a [u8], RecordError> {
        within(len, max, err)?;
        self.take(len)
    }

    fn string(&mut self, len: usize, max: usize, err: RecordError) -> Result<String, RecordError> {
        let bytes = self.bounded(len, max, err)?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| RecordError::NotUtf8)
    }

    /// Check a list count: against its bound, then against the remaining
    /// input at the smallest size an element can have. Only then is the list
    /// allocated.
    fn count(
        &self,
        count: usize,
        max: usize,
        min_element_len: usize,
        err: RecordError,
    ) -> Result<usize, RecordError> {
        within(count, max, err)?;
        if count.saturating_mul(min_element_len) > self.rest.len() {
            return Err(RecordError::Truncated);
        }
        Ok(count)
    }

    fn wire<T>(&mut self, from_wire: fn(u8) -> Option<T>) -> Result<T, RecordError> {
        from_wire(self.u8()?).ok_or(RecordError::UnknownOption)
    }
}

fn read_thumb(r: &mut Reader<'_>) -> Result<VaultThumb, RecordError> {
    let blob_id = r.array()?;
    let key = r.array()?;
    let nonce = r.array()?;
    let mime_len = r.len_u16()?;
    let mime = r.string(mime_len, MAX_MIME_BYTES, RecordError::Attachment)?;
    Ok(VaultThumb {
        blob_id,
        key,
        nonce,
        mime,
        size: r.u32()?,
        width: r.opt_u16()?,
        height: r.opt_u16()?,
    })
}

fn read_attachment(r: &mut Reader<'_>) -> Result<VaultAttachment, RecordError> {
    let blob_id = r.array()?;
    let key = r.array()?;
    let nonce = r.array()?;
    let size = r.u64()?;
    let mime_len = r.len_u16()?;
    let mime = r.string(mime_len, MAX_MIME_BYTES, RecordError::Attachment)?;
    let name_len = r.len_u16()?;
    let name = r.string(name_len, MAX_NAME_BYTES, RecordError::Attachment)?;
    let kind_len = r.len_u8()?;
    let kind = r.string(kind_len, MAX_KIND_BYTES, RecordError::Attachment)?;
    let duration_ms = r.opt_u32()?;
    let waveform_len = r.len_u8()?;
    let waveform = r
        .bounded(waveform_len, MAX_WAVEFORM_BYTES, RecordError::Attachment)?
        .to_vec();
    let width = r.opt_u16()?;
    let height = r.opt_u16()?;
    let thumb = if r.bool()? {
        Some(read_thumb(r)?)
    } else {
        None
    };
    let title_len = r.len_u8()?;
    let title = r.string(title_len, MAX_AUDIO_TAG_BYTES, RecordError::Attachment)?;
    let artist_len = r.len_u8()?;
    let artist = r.string(artist_len, MAX_AUDIO_TAG_BYTES, RecordError::Attachment)?;
    Ok(VaultAttachment {
        blob_id,
        key,
        nonce,
        size,
        mime,
        name,
        kind,
        duration_ms,
        waveform,
        width,
        height,
        thumb,
        title,
        artist,
    })
}

fn read_reply(r: &mut Reader<'_>) -> Result<VaultReply, RecordError> {
    let target = r.u64()?;
    let author_len = r.len_u16()?;
    let author = r.string(author_len, MAX_REPLY_AUTHOR_BYTES, RecordError::Reply)?;
    let excerpt_len = r.len_u16()?;
    let excerpt = r.string(excerpt_len, MAX_REPLY_EXCERPT_BYTES, RecordError::Reply)?;
    Ok(VaultReply {
        target,
        author,
        excerpt,
    })
}

fn read_forward(r: &mut Reader<'_>) -> Result<VaultForward, RecordError> {
    let author_len = r.len_u16()?;
    let author = r.string(author_len, MAX_FORWARD_AUTHOR_BYTES, RecordError::Forward)?;
    let origin = if r.bool()? {
        let group_len = r.len_u8()?;
        if group_len == 0 {
            return Err(RecordError::Forward);
        }
        let group_id = r
            .bounded(
                group_len,
                MAX_FORWARD_ORIGIN_GROUP_BYTES,
                RecordError::Forward,
            )?
            .to_vec();
        Some(VaultForwardOrigin {
            group_id,
            message_id: r.u64()?,
        })
    } else {
        None
    };
    Ok(VaultForward { author, origin })
}

fn read_reactions(r: &mut Reader<'_>) -> Result<Vec<VaultReaction>, RecordError> {
    let declared = r.len_u16()?;
    let count = r.count(
        declared,
        MAX_REACTIONS,
        MIN_REACTION_LEN,
        RecordError::Reactions,
    )?;
    let mut reactions = Vec::with_capacity(count);
    for _ in 0..count {
        let reactor = r.array()?;
        let emoji_len = r.len_u8()?;
        if emoji_len == 0 {
            return Err(RecordError::Reactions);
        }
        let emoji = r.string(emoji_len, MAX_EMOJI_BYTES, RecordError::Reactions)?;
        reactions.push(VaultReaction { reactor, emoji });
    }
    Ok(reactions)
}

fn read_message(r: &mut Reader<'_>) -> Result<VaultMessage, RecordError> {
    let message_id = r.u64()?;
    let direction = r.wire(Direction::from_wire)?;
    let author = if r.bool()? { Some(r.array()?) } else { None };
    let created_at_ms = r.i64()?;
    let last_op_id = r.u64()?;
    let flags = r.u8()?;
    if flags & !FLAG_KNOWN != 0 {
        return Err(RecordError::UnknownFlag);
    }
    let text_len = r.len_u32()?;
    let text = r.string(text_len, MAX_TEXT_BYTES, RecordError::Text)?;
    let declared = r.len_u8()?;
    let attachment_count = r.count(
        declared,
        MAX_ATTACHMENTS,
        MIN_ATTACHMENT_LEN,
        RecordError::Attachments,
    )?;
    let mut attachments = Vec::with_capacity(attachment_count);
    for _ in 0..attachment_count {
        attachments.push(read_attachment(r)?);
    }
    let reply = if r.bool()? {
        Some(read_reply(r)?)
    } else {
        None
    };
    let forward = if r.bool()? {
        Some(read_forward(r)?)
    } else {
        None
    };
    let reactions = read_reactions(r)?;
    let call = if r.bool()? {
        Some(VaultCall {
            outcome: r.u8()?,
            video: r.bool()?,
            duration_secs: r.u32()?,
        })
    } else {
        None
    };
    Ok(VaultMessage {
        message_id,
        direction,
        author,
        created_at_ms,
        last_op_id,
        edited: flags & FLAG_EDITED != 0,
        deleted: flags & FLAG_DELETED != 0,
        text,
        attachments,
        reply,
        forward,
        reactions,
        call,
        unsupported_version: r.opt_u32()?,
        content: read_content(r)?,
    })
}

fn read_content(r: &mut Reader<'_>) -> Result<Option<VaultContent>, RecordError> {
    if !r.bool()? {
        return Ok(None);
    }
    let kind = r.u8()?;
    if !CONTENT_KINDS.contains(&kind) {
        return Err(RecordError::Content);
    }
    let payload_len = r.len_u32()?;
    if payload_len == 0 {
        return Err(RecordError::Content);
    }
    let payload = r
        .bounded(payload_len, MAX_CONTENT_PAYLOAD_BYTES, RecordError::Content)?
        .to_vec();
    let state_len = r.len_u16()?;
    let state = r
        .bounded(state_len, MAX_CONTENT_STATE_BYTES, RecordError::Content)?
        .to_vec();
    Ok(Some(VaultContent {
        kind,
        payload,
        state,
    }))
}

fn read_messages(r: &mut Reader<'_>, max: usize) -> Result<Vec<VaultMessage>, RecordError> {
    let declared = r.len_u16()?;
    let count = r.count(declared, max, MIN_MESSAGE_LEN, RecordError::Messages)?;
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        messages.push(read_message(r)?);
    }
    Ok(messages)
}

fn read_ref(r: &mut Reader<'_>) -> Result<ConversationRef, RecordError> {
    if r.u8()? != REF_TAG_CONVERSATION {
        return Err(RecordError::Ref);
    }
    let kind = r.u8()?;
    let id_len = r.len_u8()?;
    let id = r.bounded(id_len, MAX_CONVERSATION_ID_LEN, RecordError::Ref)?;
    ConversationRef::from_wire(kind, id).map_err(|_| RecordError::Ref)
}

fn read_reentry(r: &mut Reader<'_>) -> Result<Vec<ReentryEpoch>, RecordError> {
    let declared = r.len_u8()?;
    let count = r.count(
        declared,
        MAX_REENTRY_EPOCHS,
        MIN_REENTRY_LEN,
        RecordError::Reentry,
    )?;
    let mut reentry = Vec::with_capacity(count);
    for _ in 0..count {
        let epoch = r.u64()?;
        let info_len = r.len_u32()?;
        if info_len == 0 {
            return Err(RecordError::Reentry);
        }
        let group_info = r
            .bounded(info_len, MAX_GROUP_INFO_BYTES, RecordError::Reentry)?
            .to_vec();
        reentry.push(ReentryEpoch { epoch, group_info });
    }
    Ok(reentry)
}

fn read_people(r: &mut Reader<'_>) -> Result<Vec<VaultPerson>, RecordError> {
    let declared = r.len_u16()?;
    let count = r.count(declared, MAX_PEOPLE, MIN_PERSON_LEN, RecordError::People)?;
    let mut people = Vec::with_capacity(count);
    for _ in 0..count {
        let account_key = r.array()?;
        let name_len = r.len_u8()?;
        let display_name = r.string(name_len, MAX_DISPLAY_NAME_BYTES, RecordError::People)?;
        let username_len = r.len_u8()?;
        let username = r.string(username_len, MAX_USERNAME_BYTES, RecordError::People)?;
        people.push(VaultPerson {
            account_key,
            display_name,
            username,
            role: r.wire(Role::from_wire)?,
        });
    }
    Ok(people)
}

fn read_shared_pins(r: &mut Reader<'_>) -> Result<Vec<SharedPin>, RecordError> {
    let declared = r.len_u16()?;
    let count = r.count(declared, MAX_PINS, SHARED_PIN_LEN, RecordError::Pins)?;
    let mut pins = Vec::with_capacity(count);
    for _ in 0..count {
        pins.push(SharedPin {
            message_id: r.u64()?,
            pinned: r.bool()?,
            updated_at_ms: r.u64()?,
        });
    }
    Ok(pins)
}

fn read_personal_pins(r: &mut Reader<'_>) -> Result<Vec<PersonalPin>, RecordError> {
    let declared = r.len_u16()?;
    let count = r.count(declared, MAX_PINS, PERSONAL_PIN_LEN, RecordError::Pins)?;
    let mut pins = Vec::with_capacity(count);
    for _ in 0..count {
        pins.push(PersonalPin {
            message_id: r.u64()?,
            pinned_at_ms: r.u64()?,
        });
    }
    Ok(pins)
}

fn read_chunk_table(r: &mut Reader<'_>) -> Result<Vec<ChunkEntry>, RecordError> {
    let declared = r.len_u32()?;
    let count = r.count(declared, MAX_CHUNKS, CHUNK_ENTRY_LEN, RecordError::Chunks)?;
    let mut chunks = Vec::with_capacity(count);
    for _ in 0..count {
        chunks.push(ChunkEntry {
            count: r.u16()?,
            first_id: r.u64()?,
            last_id: r.u64()?,
            version: r.u64()?,
        });
    }
    Ok(chunks)
}

impl HistoryChunk {
    /// Decode a chunk, refusing anything [`Self::encode`] would not produce.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first thing that is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader::open(bytes)?;
        let messages = read_messages(&mut r, MAX_CHUNK_MESSAGES)?;
        r.finish()?;
        let chunk = Self { messages };
        chunk.validate()?;
        Ok(chunk)
    }
}

impl ConversationHead {
    /// Decode a head, refusing anything [`Self::encode`] would not produce.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first thing that is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader::open(bytes)?;
        let conversation = read_ref(&mut r)?;
        let kind = r.wire(ConversationKind::from_wire)?;
        let title_len = r.len_u16()?;
        let title = r.string(title_len, MAX_TITLE_BYTES, RecordError::Title)?;
        let people = read_people(&mut r)?;
        let membership = r.wire(Membership::from_wire)?;
        let shared_pins = read_shared_pins(&mut r)?;
        let personal_pins = read_personal_pins(&mut r)?;
        let last_activity_ms = r.u64()?;
        let chunks = read_chunk_table(&mut r)?;
        let tail = read_messages(&mut r, MAX_TAIL_MESSAGES)?;
        let reentry = read_reentry(&mut r)?;
        let registrar_seed = if r.bool()? {
            Some(RegistrarSeed {
                rank: r.u8()?,
                seed: SecretBytes::new(r.array()?),
            })
        } else {
            None
        };
        r.finish()?;
        let head = Self {
            conversation,
            kind,
            title,
            people,
            membership,
            shared_pins,
            personal_pins,
            last_activity_ms,
            chunks,
            tail,
            reentry,
            registrar_seed,
        };
        head.validate()?;
        Ok(head)
    }
}

impl DevicesHead {
    /// Decode a devices head, refusing anything [`Self::encode`] would not
    /// produce.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first thing that is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader::open(bytes)?;
        let reentry = read_reentry(&mut r)?;
        r.finish()?;
        let head = Self { reentry };
        head.validate()?;
        Ok(head)
    }
}

impl VaultIndex {
    /// Decode an index, refusing anything [`Self::encode`] would not
    /// produce.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first thing that is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader::open(bytes)?;
        let declared = r.len_u32()?;
        let count = r.count(
            declared,
            MAX_INDEX_ENTRIES,
            MIN_INDEX_ENTRY_LEN,
            RecordError::Entries,
        )?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(IndexEntry {
                conversation: read_ref(&mut r)?,
                last_activity_ms: r.u64()?,
                membership: r.wire(Membership::from_wire)?,
            });
        }
        r.finish()?;
        let index = Self { entries };
        index.validate()?;
        Ok(index)
    }
}

#[cfg(test)]
mod tests;
