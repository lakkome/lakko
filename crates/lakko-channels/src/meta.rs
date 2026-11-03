// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel metadata record — a channel's name and description, carried
//! end-to-end encrypted to every subscriber
//! ([ADR-0051 §4](../../../docs/adr/0051-group-and-channel-names.md)).
//!
//! ## How it travels
//!
//! The record is the **plaintext of an ordinary channel post**. It goes
//! through [`crate::post::compose_post`] like any post: it consumes a
//! counter from the publisher's monotone sequence, is sealed under the
//! epoch seed and signed by the publisher key. So authenticity is the post
//! signature (only a publisher can post) and confidentiality is the epoch
//! seed; this module adds no cryptography of its own. The server relays an
//! opaque post and learns nothing (invariants #1, #2).
//!
//! Every other post's plaintext is the **chat envelope**
//! (`lakko_attachments`, ADR-0087 section 13): text and attachment
//! references, ADR-0047 metadata included, and the ADR-0023 edit and delete
//! ops. A reader runs [`classify_post_plaintext`] on every decrypted post:
//! the record's domain separator is checked first, and anything else goes to
//! `lakko_attachments::parse_body`, so a post and a chat message share one
//! codec, one bound set and one inline-media rule.
//!
//! ## Why a chat envelope cannot collide with the record
//!
//! [`CHANNEL_META_DOMAIN_V1`] ends in a NUL byte, and a chat envelope is JSON
//! that opens with `{`. The domain bytes are frozen: the in-payload version
//! byte is the knob that moves.
//!
//! ## The ban list beside it
//!
//! A public channel's ban list ([`crate::ban_list`], ADR-0087 section 13a)
//! travels the same way under its own NUL-terminated domain, and
//! [`classify_post_plaintext`] recognises it before the metadata domain:
//! [`PostContent::BanList`] or [`PostContent::MalformedBanList`], never shown
//! as a post.
//!
//! ## What a channel does not take
//!
//! Typing signals, read receipts and chat reactions do not exist in a
//! channel (reactions are ADR-0087 section 13a, at the channel service). A
//! post whose body is one of those is [`PostContent::NotAPost`] and is
//! dropped. A reply's quote is not read either: the reader shows the post.
//!
//! ## What readers do with it
//!
//! * A reader keeps the **newest** record per channel by `updated_at_ms`
//!   ([`channel_meta_is_newer`] — strictly greater; equal is not newer) and
//!   titles the channel from it. A record is never shown as a post.
//! * A domain match that fails any bound is [`PostContent::Malformed`]; the
//!   caller **drops** it and never renders it as text (fail-closed).
//! * No domain match is [`PostContent::Message`] with the parsed body (or
//!   [`PostContent::NotAPost`]).
//!
//! ## When the publisher (re-)issues it
//!
//! At channel creation, on every edit, and **after every rotation** — a
//! subscriber holds seeds for a bounded window of epochs (ADR-0031), so the
//! newest epoch must always hold a record a late subscriber can read. A
//! re-issue carries the **stored** `updated_at_ms` verbatim (the ADR-0044
//! lesson): re-stamping at send time would let a re-issue outrank a newer
//! edit from a co-publisher.
//!
//! ## Wire shape (schema version 3, ADR-0087 sections 11 and 16)
//!
//! ```text
//!   CHANNEL_META_DOMAIN_V1 || version || updated_at_ms || name_len || name || description_len || description
//!   └── 22 bytes, ASCII ──┘ └─ 0x03 ─┘ └── 8 B, BE ───┘ └─ 1 B ──┘ └ .. ┘ └───── 2 B, BE ─────┘ └──── .. ────┘
//!
//!     || topic || language_len || language || public || handle_len || handle
//!        └ 1 B ┘ └─── 1 B ────┘ └ 0/2/3 B ┘ └ 1 B ─┘ └── 2 B ───┘ └ 0 or a conversation-handle envelope ┘
//!
//!     || reactions_mode || [count || count x (emoji_len || emoji)]
//!        └──── 1 B ────┘   └ 1 B ┘           └─ 1 B ──┘ └ 1..=64 B ┘
//!
//!                     "lakko/channel-meta/v1\x00"
//! ```
//!
//! Version 3 is a **flag day**: versions `0x01` and `0x02` are refused like
//! any other unknown version (no production data exists).
//!
//! * `topic` — a [`crate::listing::TOPICS`] id. Any byte decodes; a byte
//!   the reader does not know is shown as "Other"
//!   ([`crate::listing::topic_name`]), so a later topic list stays readable
//!   by an older client. The byte is kept verbatim, never rewritten, so the
//!   encoding stays canonical.
//! * `language` — **empty means "not set"**; otherwise an ISO 639 code of 2
//!   or 3 ASCII lowercase letters ([`crate::listing::check_language`]). The
//!   metadata record admits an unset language because a channel that is not
//!   listed has no need to state one; the public listing
//!   ([`crate::listing`]) requires it.
//! * `public` — `0x00` private or `0x01` public, any other byte refuses the
//!   record. ADR-0087 section 16 folds ADR-0059's `listed` into it: a public
//!   channel is found by name and listed, a private one is neither, so a
//!   member shares only public channels in a group.
//! * `handle` — the channel's `@name` as a complete conversation-handle
//!   envelope (`lakko_contacts::conversation_handle`, ADR-0087 section 11),
//!   signed by the owner's handle key over the **channel id** (16 bytes) in
//!   the place of a group id, and carried verbatim by a co-publisher's
//!   re-issue. Empty while the channel has no name (a lapsed lease). Its
//!   flag must equal `public`. The reader verifies the certificate and the
//!   signature as for a group (one OPRF evaluation over the name, then
//!   `ConversationHandle::verify_signature(channel_id, handle_key)`); this
//!   module only frames it.
//!
//! * `reactions_mode` — which reactions the channel's posts take
//!   ([`AllowedReactions`], maintainer 2026-11-02, like a Telegram channel's
//!   setting): `0x00` all, `0x01` none, `0x02` the list that follows, 1 to
//!   [`ALLOWED_REACTIONS_MAX`] (100, chosen) distinct emoji each passing the
//!   chat reaction rule (`lakko_attachments::is_reaction_emoji`). Any other
//!   byte refuses the record. **The channel service cannot enforce it**: it
//!   sees labels only, never an emoji. Readers drop the counts of reactions
//!   the record does not allow and the composer does not offer them.
//!
//! Total length is between [`CHANNEL_META_MIN_LEN`] and
//! [`CHANNEL_META_MAX_LEN`].
//!
//! ## Bounds, and why a violation rejects the whole record
//!
//! * `name`: valid UTF-8, trimmed, 1 to [`NAME_MAX_CHARS`] Unicode scalar
//!   values and at most [`NAME_MAX_BYTES`] bytes (the one-byte `name_len`
//!   ceiling), no line break, no control character (`char::is_control`),
//!   no bidi or invisible format character (see below).
//! * `description`: valid UTF-8, trimmed, 0 to [`DESCRIPTION_MAX_CHARS`]
//!   scalars and at most [`DESCRIPTION_MAX_BYTES`] bytes, LINE FEED
//!   permitted (a paragraph may break lines), every other rule the same.
//!   The only permitted break is `\n`; a caller normalises `\r\n` first.
//!
//! A record violating any bound is **rejected as a whole**: a good name
//! beside a description smuggling a right-to-left override is not a record
//! anyone should render half of. **`compose` refuses exactly what `decode`
//! refuses** — both run [`ChannelMeta::validate`].
//!
//! ### The format-character table
//!
//! The field rules are defined once in `lakko_contacts::channel_text` and
//! re-exported here under their historical names, because the directory
//! listing and the group envelope that shares followed channels (ADR-0059)
//! must refuse exactly what this record refuses. The rule is deliberately
//! narrower than the profile display-name rule (the full general-category
//! `Cf` table): [`REFUSED_FORMAT_CHAR_RANGES`] lists the subset that can
//! mislead a reader — the bidi controls, which reorder how neighbouring text
//! renders, and the invisible spacers (ZWSP, WORD JOINER and its siblings,
//! BOM, SOFT HYPHEN) that no name needs. ZERO WIDTH NON-JOINER (U+200C) and
//! ZERO WIDTH JOINER (U+200D) are **permitted**: ZWJ builds emoji sequences
//! and ZWNJ is required by Persian and several Indic spellings.

use core::result::Result as CoreResult;

use thiserror::Error;

use lakko_attachments::{is_reaction_emoji, parse_body, MessageBody, MAX_EMOJI_BYTES};
use lakko_contacts::conversation_handle::{
    compose_conversation_handle, decode_conversation_handle, ConversationHandle,
    ConversationHandleError, CONVERSATION_HANDLE_ENVELOPE_MAX_LEN,
    CONVERSATION_HANDLE_ENVELOPE_MIN_LEN,
};

use crate::listing::{check_language, LanguageError, LANGUAGE_MAX_LEN};

pub use lakko_contacts::channel_text::{
    check_description, check_name, is_refused_format_char,
    ChannelTextError as ChannelMetaFieldError, DESCRIPTION_MAX_BYTES, DESCRIPTION_MAX_CHARS,
    NAME_MAX_BYTES, NAME_MAX_CHARS, REFUSED_FORMAT_CHAR_RANGES,
};

/// 22-byte domain separator the record starts with.
///
/// **Frozen.** The trailing `\x00` is what keeps a UTF-8 text post from
/// ever matching it (see the module docs). The printable prefix is
/// `lakko/channel-meta/v1`; the `v1` names the record FAMILY, the in-payload
/// version byte is the schema version.
pub const CHANNEL_META_DOMAIN_V1: &[u8] = b"lakko/channel-meta/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value (ADR-0087; versions `0x01` and `0x02` are refused).
pub const CHANNEL_META_VERSION_V3: u8 = 0x03;

/// Bytes after the separator that every record carries: version,
/// `updated_at_ms`, `name_len`, `description_len`, `topic`,
/// `language_len`, `public`, `handle_len`, `reactions_mode`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1 + 2 + 1 + 1 + 1 + 2 + 1;

/// The most emoji an allowed-reactions list holds. Chosen.
pub const ALLOWED_REACTIONS_MAX: usize = 100;

const REACTIONS_ALL: u8 = 0x00;
const REACTIONS_NONE: u8 = 0x01;
const REACTIONS_LIST: u8 = 0x02;

/// Which reactions a channel's posts take, as the owner set them.
///
/// Advisory by construction: the service sees labels only and cannot refuse
/// an emoji. Readers drop what is not allowed and the composer does not
/// offer it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AllowedReactions {
    /// Any reaction a chat message takes.
    #[default]
    All,
    /// No reactions.
    Off,
    /// Only these, 1 to [`ALLOWED_REACTIONS_MAX`] distinct emoji, in the
    /// owner's order (the order a composer offers them in).
    Only(Vec<String>),
}

impl AllowedReactions {
    /// Whether a post of the channel takes `emoji`.
    #[must_use]
    pub fn allows(&self, emoji: &str) -> bool {
        match self {
            Self::All => is_reaction_emoji(emoji),
            Self::Off => false,
            Self::Only(list) => list.iter().any(|e| e == emoji),
        }
    }

    /// Check the list bounds.
    ///
    /// # Errors
    ///
    /// [`ChannelMetaError::Reactions`] for an empty or over-long list, an
    /// emoji failing the chat rule, or a repeat.
    pub fn validate(&self) -> CoreResult<(), ChannelMetaError> {
        let Self::Only(list) = self else {
            return Ok(());
        };
        if list.is_empty() || list.len() > ALLOWED_REACTIONS_MAX {
            return Err(ChannelMetaError::Reactions);
        }
        for (i, e) in list.iter().enumerate() {
            if !is_reaction_emoji(e) || list[..i].contains(e) {
                return Err(ChannelMetaError::Reactions);
            }
        }
        Ok(())
    }

    fn encode_into(&self, out: &mut Vec<u8>) -> CoreResult<(), ChannelMetaError> {
        match self {
            Self::All => out.push(REACTIONS_ALL),
            Self::Off => out.push(REACTIONS_NONE),
            Self::Only(list) => {
                out.push(REACTIONS_LIST);
                out.push(u8::try_from(list.len()).map_err(|_| ChannelMetaError::Reactions)?);
                for e in list {
                    out.push(u8::try_from(e.len()).map_err(|_| ChannelMetaError::Reactions)?);
                    out.extend_from_slice(e.as_bytes());
                }
            },
        }
        Ok(())
    }

    fn take(cur: &mut &[u8]) -> CoreResult<Self, ChannelMetaError> {
        match take_u8(cur)? {
            REACTIONS_ALL => Ok(Self::All),
            REACTIONS_NONE => Ok(Self::Off),
            REACTIONS_LIST => {
                let count = usize::from(take_u8(cur)?);
                if count == 0 || count > ALLOWED_REACTIONS_MAX {
                    return Err(ChannelMetaError::Reactions);
                }
                let mut list = Vec::with_capacity(count);
                for _ in 0..count {
                    let len = usize::from(take_u8(cur)?);
                    if len == 0 || len > MAX_EMOJI_BYTES {
                        return Err(ChannelMetaError::Reactions);
                    }
                    let e = core::str::from_utf8(take(cur, len)?)
                        .map_err(|_| ChannelMetaError::Reactions)?;
                    list.push(e.to_string());
                }
                Ok(Self::Only(list))
            },
            other => Err(ChannelMetaError::ReactionsMode(other)),
        }
    }
}

/// Shortest well-formed record: a one-byte name, no description, no
/// language.
pub const CHANNEL_META_MIN_LEN: usize = CHANNEL_META_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 1;

/// Longest well-formed record: a maximal name, a maximal description, a
/// three-letter language and a maximal handle envelope.
pub const CHANNEL_META_MAX_LEN: usize = CHANNEL_META_DOMAIN_V1.len()
    + FIXED_PAYLOAD_LEN
    + NAME_MAX_BYTES
    + DESCRIPTION_MAX_BYTES
    + LANGUAGE_MAX_LEN
    + CONVERSATION_HANDLE_ENVELOPE_MAX_LEN
    + 1
    + ALLOWED_REACTIONS_MAX * (1 + MAX_EMOJI_BYTES);

/// A channel's name and description, as the publisher stated them.
///
/// Both fields are the publisher's own statement; the receiver trusts them
/// because the carrying post's signature verified, not because of anything
/// in this record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMeta {
    /// The channel name. Must satisfy [`check_name`]: trimmed, never empty.
    pub name: String,
    /// The channel description. Must satisfy [`check_description`]: trimmed,
    /// may be empty, may contain LINE FEED.
    pub description: String,
    /// Publisher-clock UNIX milliseconds at which this record was set.
    /// Orders records for one channel via [`channel_meta_is_newer`].
    pub updated_at_ms: u64,
    /// The channel's topic, a [`crate::listing::TOPICS`] id. Any byte is
    /// admissible; an unknown one reads as "Other".
    pub topic: u8,
    /// ISO 639 language code, 2 or 3 ASCII lowercase letters, or **empty
    /// for "not set"**.
    pub language: String,
    /// Public (found by name, listed, read by anyone who subscribes) or
    /// private (ADR-0087 sections 6, 7 and 16).
    pub public: bool,
    /// The channel's `@name`, as the owner's handle key signed it for this
    /// channel id; `None` while it has none. Its flag must equal
    /// [`Self::public`].
    pub handle: Option<ConversationHandle>,
    /// Which reactions the channel's posts take (advisory; see
    /// [`AllowedReactions`]).
    pub reactions: AllowedReactions,
}

impl ChannelMeta {
    /// Check every bound. The single rule both [`compose_channel_meta`] and
    /// [`decode_channel_meta`] run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// * [`ChannelMetaError::Name`] — the name violates a bound.
    /// * [`ChannelMetaError::Description`] — the description violates a
    ///   bound.
    /// * [`ChannelMetaError::Language`] — the language is neither empty nor
    ///   2-3 ASCII lowercase letters.
    /// * [`ChannelMetaError::Handle`] — the handle envelope does not compose.
    /// * [`ChannelMetaError::HandleFlagMismatch`] — the handle's flag is not
    ///   [`Self::public`].
    pub fn validate(&self) -> CoreResult<(), ChannelMetaError> {
        check_name(&self.name).map_err(ChannelMetaError::Name)?;
        check_description(&self.description).map_err(ChannelMetaError::Description)?;
        if !self.language.is_empty() {
            check_language(&self.language).map_err(ChannelMetaError::Language)?;
        }
        if let Some(handle) = &self.handle {
            compose_conversation_handle(handle).map_err(ChannelMetaError::Handle)?;
            if handle.public != self.public {
                return Err(ChannelMetaError::HandleFlagMismatch);
            }
        }
        self.reactions.validate()
    }
}

/// Why a record was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelMetaError {
    /// The plaintext does not start with [`CHANNEL_META_DOMAIN_V1`]. Only
    /// from [`decode_channel_meta`]; [`classify_post_plaintext`] returns
    /// [`PostContent::Text`] instead.
    #[error("plaintext does not carry the channel-meta domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("channel-meta record is truncated")]
    Truncated,
    /// Bytes remain after the description.
    #[error("channel-meta record has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the description.
        count: usize,
    },
    /// The version byte is not [`CHANNEL_META_VERSION_V3`].
    #[error("channel-meta record carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The `public` byte is neither `0x00` nor `0x01`.
    #[error("channel-meta record carries an invalid public flag {0:#04x}")]
    PublicFlag(u8),
    /// The handle envelope is malformed, or its length is outside the
    /// envelope bounds.
    #[error("invalid channel handle: {0}")]
    Handle(ConversationHandleError),
    /// The handle envelope's flag is not the record's `public` flag.
    #[error("channel handle flag does not match the record")]
    HandleFlagMismatch,
    /// The language is neither empty nor an ISO 639 code.
    #[error("invalid channel language: {0}")]
    Language(LanguageError),
    /// The name violates a bound.
    #[error("invalid channel name: {0}")]
    Name(ChannelMetaFieldError),
    /// The description violates a bound.
    #[error("invalid channel description: {0}")]
    Description(ChannelMetaFieldError),
    /// The `reactions_mode` byte is none of all, none and list.
    #[error("channel-meta record carries an unknown reactions mode {0:#04x}")]
    ReactionsMode(u8),
    /// The allowed-reactions list is empty, longer than
    /// [`ALLOWED_REACTIONS_MAX`], repeats an emoji, or holds one failing the
    /// chat reaction rule.
    #[error("invalid allowed-reactions list")]
    Reactions,
}

impl ChannelMetaError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    ///
    /// Field errors are namespaced (`name.tooLong`,
    /// `description.controlOrFormatCharacter`) so one code set covers both.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::PublicFlag(_) => "publicFlag".to_string(),
            Self::Handle(e) => format!("handle.{}", e.code()),
            Self::HandleFlagMismatch => "handleFlagMismatch".to_string(),
            Self::Language(e) => format!("language.{}", e.code()),
            Self::Name(e) => format!("name.{}", e.code()),
            Self::Description(e) => format!("description.{}", e.code()),
            Self::ReactionsMode(_) => "reactionsMode".to_string(),
            Self::Reactions => "reactions".to_string(),
        }
    }
}

/// What a decrypted post plaintext turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostContent<'a> {
    /// A post: the chat envelope parsed by `lakko_attachments::parse_body`
    /// (`Chat`, `Edit`, `Delete`, or the never-rendered `ServiceEnvelope` /
    /// `Unsupported`), with the plaintext verbatim beside it.
    Message {
        /// The decrypted plaintext, as given.
        plaintext: &'a [u8],
        /// What it parsed to.
        body: MessageBody,
    },
    /// A well-formed, in-bounds metadata record.
    Meta(ChannelMeta),
    /// The domain separator matched but the record is malformed or out of
    /// bounds. The caller drops it and never renders it as text.
    Malformed(ChannelMetaError),
    /// A chat envelope a channel does not take: a typing signal, a read
    /// receipt or a chat reaction. Dropped.
    NotAPost,
    /// A well-formed ban list ([`crate::ban_list`], ADR-0087 section 13a).
    /// Never shown as a post.
    BanList(crate::ban_list::BanList),
    /// The ban-list domain matched but the list is malformed or out of
    /// bounds. Dropped.
    MalformedBanList(crate::ban_list::BanListError),
}

/// `true` when a record stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` for the **same channel**.
///
/// Strictly greater wins; **equal is not newer**, so a re-issue of the
/// record a reader already holds is not an update.
#[must_use]
pub const fn channel_meta_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a metadata record.
///
/// The output is the **plaintext** handed to [`crate::post::compose_post`];
/// it is not itself sealed or signed.
///
/// # Errors
///
/// Exactly the errors [`ChannelMeta::validate`] returns — `compose` refuses
/// what [`decode_channel_meta`] would refuse.
pub fn compose_channel_meta(meta: &ChannelMeta) -> CoreResult<Vec<u8>, ChannelMetaError> {
    meta.validate()?;
    let name = meta.name.as_bytes();
    let description = meta.description.as_bytes();
    // `validate` bounded both lengths; the conversions are re-checked rather
    // than cast so a future bound change cannot truncate silently.
    let name_len = u8::try_from(name.len()).map_err(|_| {
        ChannelMetaError::Name(ChannelMetaFieldError::TooManyBytes {
            max_bytes: NAME_MAX_BYTES,
        })
    })?;
    let description_len = u16::try_from(description.len()).map_err(|_| {
        ChannelMetaError::Description(ChannelMetaFieldError::TooManyBytes {
            max_bytes: DESCRIPTION_MAX_BYTES,
        })
    })?;

    let mut out = Vec::with_capacity(
        CHANNEL_META_DOMAIN_V1.len()
            + FIXED_PAYLOAD_LEN
            + name.len()
            + description.len()
            + meta.language.len(),
    );
    out.extend_from_slice(CHANNEL_META_DOMAIN_V1);
    out.push(CHANNEL_META_VERSION_V3);
    out.extend_from_slice(&meta.updated_at_ms.to_be_bytes());
    out.push(name_len);
    out.extend_from_slice(name);
    out.extend_from_slice(&description_len.to_be_bytes());
    out.extend_from_slice(description);
    out.push(meta.topic);
    // `validate` bounded the language to 0, 2 or 3 bytes.
    let language = meta.language.as_bytes();
    out.push(
        u8::try_from(language.len())
            .map_err(|_| ChannelMetaError::Language(LanguageError::BadLength))?,
    );
    out.extend_from_slice(language);
    out.push(u8::from(meta.public));
    let handle = match &meta.handle {
        Some(h) => compose_conversation_handle(h).map_err(ChannelMetaError::Handle)?,
        None => Vec::new(),
    };
    // A composed envelope is at most CONVERSATION_HANDLE_ENVELOPE_MAX_LEN.
    let handle_len = u16::try_from(handle.len())
        .map_err(|_| ChannelMetaError::Handle(ConversationHandleError::BadLength))?;
    out.extend_from_slice(&handle_len.to_be_bytes());
    out.extend_from_slice(&handle);
    meta.reactions.encode_into(&mut out)?;
    Ok(out)
}

/// Strict decoder: a plaintext that is not a record is a hard error.
///
/// Readers normally call [`classify_post_plaintext`], which falls through
/// to text on a non-matching domain; this is for a caller that already
/// knows it holds a record.
///
/// # Errors
///
/// * [`ChannelMetaError::WrongDomain`] — `bytes` does not start with
///   [`CHANNEL_META_DOMAIN_V1`].
/// * [`ChannelMetaError::Truncated`] — the payload ends before a declared
///   field (a bare separator included).
/// * [`ChannelMetaError::UnknownVersion`] — the version byte is not
///   [`CHANNEL_META_VERSION_V3`].
/// * [`ChannelMetaError::TrailingBytes`] — bytes remain after the
///   description.
/// * [`ChannelMetaError::Name`] / [`ChannelMetaError::Description`] — a
///   field is out of bounds.
pub fn decode_channel_meta(bytes: &[u8]) -> CoreResult<ChannelMeta, ChannelMetaError> {
    let payload = bytes
        .strip_prefix(CHANNEL_META_DOMAIN_V1)
        .ok_or(ChannelMetaError::WrongDomain)?;
    decode_payload(payload)
}

/// Reader-side classifier: run this on every decrypted post plaintext.
///
/// The two record domains are checked first. A metadata match that decodes
/// within every bound is [`PostContent::Meta`]; a match that fails any bound
/// is [`PostContent::Malformed`], which the caller drops. A ban-list match
/// ([`crate::ban_list`]) is [`PostContent::BanList`] or
/// [`PostContent::MalformedBanList`], likewise never shown. No match goes to
/// `lakko_attachments::parse_body`: [`PostContent::Message`], or
/// [`PostContent::NotAPost`] for a typing signal, a read receipt or a chat
/// reaction.
#[must_use]
pub fn classify_post_plaintext(plaintext: &[u8]) -> PostContent<'_> {
    if let Some(payload) = plaintext.strip_prefix(crate::ban_list::BAN_LIST_DOMAIN_V1) {
        return match crate::ban_list::decode_payload(payload) {
            Ok(list) => PostContent::BanList(list),
            Err(e) => PostContent::MalformedBanList(e),
        };
    }
    let Some(payload) = plaintext.strip_prefix(CHANNEL_META_DOMAIN_V1) else {
        return match parse_body(plaintext) {
            MessageBody::Typing { .. }
            | MessageBody::ReadReceipt { .. }
            | MessageBody::Reaction { .. } => PostContent::NotAPost,
            body => PostContent::Message { plaintext, body },
        };
    };
    match decode_payload(payload) {
        Ok(meta) => PostContent::Meta(meta),
        Err(e) => PostContent::Malformed(e),
    }
}

fn decode_payload(payload: &[u8]) -> CoreResult<ChannelMeta, ChannelMetaError> {
    let mut cur = payload;

    let version = take_u8(&mut cur)?;
    if version != CHANNEL_META_VERSION_V3 {
        return Err(ChannelMetaError::UnknownVersion(version));
    }
    let updated_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| ChannelMetaError::Truncated)?;
    let updated_at_ms = u64::from_be_bytes(updated_at_bytes);

    let name_len = usize::from(take_u8(&mut cur)?);
    let name_bytes = take(&mut cur, name_len)?;
    let description_len_bytes: [u8; 2] = take(&mut cur, 2)?
        .try_into()
        .map_err(|_| ChannelMetaError::Truncated)?;
    let description_len = usize::from(u16::from_be_bytes(description_len_bytes));
    let description_bytes = take(&mut cur, description_len)?;
    let topic = take_u8(&mut cur)?;
    let language_len = usize::from(take_u8(&mut cur)?);
    let language_bytes = take(&mut cur, language_len)?;
    let public = match take_u8(&mut cur)? {
        0 => false,
        1 => true,
        other => return Err(ChannelMetaError::PublicFlag(other)),
    };
    let handle_len_bytes: [u8; 2] = take(&mut cur, 2)?
        .try_into()
        .map_err(|_| ChannelMetaError::Truncated)?;
    let handle_len = usize::from(u16::from_be_bytes(handle_len_bytes));
    if handle_len != 0
        && !(CONVERSATION_HANDLE_ENVELOPE_MIN_LEN..=CONVERSATION_HANDLE_ENVELOPE_MAX_LEN)
            .contains(&handle_len)
    {
        return Err(ChannelMetaError::Handle(ConversationHandleError::BadLength));
    }
    let handle_bytes = take(&mut cur, handle_len)?;
    let reactions = AllowedReactions::take(&mut cur)?;
    if !cur.is_empty() {
        return Err(ChannelMetaError::TrailingBytes { count: cur.len() });
    }

    let name = core::str::from_utf8(name_bytes)
        .map_err(|_| ChannelMetaError::Name(ChannelMetaFieldError::NotUtf8))?;
    let description = core::str::from_utf8(description_bytes)
        .map_err(|_| ChannelMetaError::Description(ChannelMetaFieldError::NotUtf8))?;

    let language = core::str::from_utf8(language_bytes)
        .map_err(|_| ChannelMetaError::Language(LanguageError::NotLowercaseAscii))?;

    let handle = if handle_bytes.is_empty() {
        None
    } else {
        Some(decode_conversation_handle(handle_bytes).map_err(ChannelMetaError::Handle)?)
    };

    let meta = ChannelMeta {
        name: name.to_string(),
        description: description.to_string(),
        updated_at_ms,
        topic,
        language: language.to_string(),
        public,
        handle,
        reactions,
    };
    meta.validate()?;
    Ok(meta)
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, ChannelMetaError> {
    let (&first, rest) = cur.split_first().ok_or(ChannelMetaError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], ChannelMetaError> {
    if cur.len() < n {
        return Err(ChannelMetaError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, description: &str, at: u64) -> ChannelMeta {
        ChannelMeta {
            name: name.to_string(),
            description: description.to_string(),
            updated_at_ms: at,
            topic: 0,
            language: String::new(),
            public: false,
            handle: None,
            reactions: AllowedReactions::All,
        }
    }

    /// Hand-build a record with arbitrary (possibly invalid) fields,
    /// bypassing `compose`'s validation, so decode's own gates are tested.
    fn raw(version: u8, at: u64, name: &[u8], description: &[u8]) -> Vec<u8> {
        let mut out = CHANNEL_META_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&at.to_be_bytes());
        out.push(u8::try_from(name.len()).unwrap());
        out.extend_from_slice(name);
        out.extend_from_slice(&u16::try_from(description.len()).unwrap().to_be_bytes());
        out.extend_from_slice(description);
        // topic, empty language, private, no handle, all reactions
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        out
    }

    // --- wire format ---------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        for m in [
            meta(
                "Lakko News",
                "Release notes and outages.",
                1_700_000_000_000,
            ),
            meta("N", "", 0),
            meta("Новости Lakko", "Первая строка\nвторая строка", u64::MAX),
            meta("\u{1F600} Party", "line 1\n\nline 3", 7),
            meta(
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} Family",
                "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
                1,
            ),
        ] {
            let wire = compose_channel_meta(&m).unwrap();
            assert_eq!(decode_channel_meta(&wire).unwrap(), m);
            assert_eq!(classify_post_plaintext(&wire), PostContent::Meta(m));
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire = compose_channel_meta(&meta("Al", "abc", 0x0102_0304_0506_0708)).unwrap();
        let sep = CHANNEL_META_DOMAIN_V1.len();
        assert_eq!(sep, 22);
        assert_eq!(&wire[..sep], b"lakko/channel-meta/v1\x00");
        assert_eq!(wire[sep], CHANNEL_META_VERSION_V3);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 2);
        assert_eq!(&wire[sep + 10..sep + 12], b"Al");
        assert_eq!(&wire[sep + 12..sep + 14], &[0, 3]);
        assert_eq!(&wire[sep + 14..sep + 17], b"abc");
        assert_eq!(&wire[sep + 17..], &[0, 0, 0, 0, 0, 0]);

        let v3 = ChannelMeta {
            topic: 9,
            language: "fil".to_string(),
            public: true,
            ..meta("Al", "", 0)
        };
        let wire = compose_channel_meta(&v3).unwrap();
        assert_eq!(&wire[sep + 14..], &[9, 3, b'f', b'i', b'l', 1, 0, 0, 0]);
        assert_eq!(decode_channel_meta(&wire).unwrap(), v3);
    }

    #[test]
    fn versions_one_and_two_are_refused_after_the_flag_day() {
        let wire = compose_channel_meta(&meta("Lakko", "", 1)).unwrap();
        for old in [0x01, 0x02] {
            let mut v = wire.clone();
            v[CHANNEL_META_DOMAIN_V1.len()] = old;
            assert_eq!(
                decode_channel_meta(&v),
                Err(ChannelMetaError::UnknownVersion(old))
            );
        }
    }

    #[test]
    fn public_flag_and_language_are_bounded() {
        let mut wire = compose_channel_meta(&meta("Lakko", "", 1)).unwrap();
        // public, handle_len(2), reactions_mode follow.
        let flag_at = wire.len() - 4;
        wire[flag_at] = 2;
        assert_eq!(
            decode_channel_meta(&wire),
            Err(ChannelMetaError::PublicFlag(2))
        );
        for (language, code) in [
            ("e", "language.badLength"),
            ("engl", "language.badLength"),
            ("EN", "language.notLowercaseAscii"),
        ] {
            let bad = ChannelMeta {
                language: language.to_string(),
                ..meta("Lakko", "", 1)
            };
            assert_eq!(compose_channel_meta(&bad).unwrap_err().code(), code);
        }
        // An unknown topic byte is kept verbatim.
        let unknown = ChannelMeta {
            topic: 250,
            ..meta("Lakko", "", 1)
        };
        let wire = compose_channel_meta(&unknown).unwrap();
        assert_eq!(decode_channel_meta(&wire).unwrap().topic, 250);
    }

    #[test]
    fn length_bounds_are_the_documented_construction() {
        assert_eq!(CHANNEL_META_MIN_LEN, 22 + 18 + 1);
        assert_eq!(
            CHANNEL_META_MAX_LEN,
            22 + 18 + 255 + 1024 + 3 + CONVERSATION_HANDLE_ENVELOPE_MAX_LEN + 1 + 100 * 65
        );
        let min = compose_channel_meta(&meta("a", "", 0)).unwrap();
        assert_eq!(min.len(), CHANNEL_META_MIN_LEN);
        // A maximal record: 255 bytes of name in exactly 64 chars (63
        // four-byte scalars + one three-byte scalar) and 1024 bytes of
        // description in 256 four-byte scalars.
        let name = format!("{}\u{20AC}", "\u{1F600}".repeat(63));
        assert_eq!(name.len(), NAME_MAX_BYTES);
        assert_eq!(name.chars().count(), NAME_MAX_CHARS);
        let description = "\u{1F600}".repeat(256);
        assert_eq!(description.len(), DESCRIPTION_MAX_BYTES);
        let max = compose_channel_meta(&ChannelMeta {
            language: "eng".to_string(),
            public: true,
            handle: Some(handle(true, &"n".repeat(32))),
            reactions: AllowedReactions::Only(
                (0u8..100)
                    .map(|i| format!("{:02}{}", i, "x".repeat(62)))
                    .collect(),
            ),
            ..meta(&name, &description, 0)
        })
        .unwrap();
        assert_eq!(max.len(), CHANNEL_META_MAX_LEN);
    }

    // --- fail-closed decode --------------------------------------------------

    #[test]
    fn non_record_plaintext_is_a_wrong_domain_for_decode() {
        assert_eq!(
            decode_channel_meta(b"hello channel"),
            Err(ChannelMetaError::WrongDomain)
        );
        assert_eq!(decode_channel_meta(&[]), Err(ChannelMetaError::WrongDomain));
    }

    #[test]
    fn bare_separator_is_truncated_not_text() {
        assert_eq!(
            decode_channel_meta(CHANNEL_META_DOMAIN_V1),
            Err(ChannelMetaError::Truncated)
        );
        assert_eq!(
            classify_post_plaintext(CHANNEL_META_DOMAIN_V1),
            PostContent::Malformed(ChannelMetaError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_channel_meta(&meta("Lakko", "news", 9)).unwrap();
        for cut in CHANNEL_META_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                decode_channel_meta(&wire[..cut]),
                Err(ChannelMetaError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut wire = compose_channel_meta(&meta("Lakko", "", 9)).unwrap();
        wire.push(0);
        assert_eq!(
            decode_channel_meta(&wire),
            Err(ChannelMetaError::TrailingBytes { count: 1 })
        );
    }

    #[test]
    fn unknown_versions_are_refused() {
        for v in [0x00, 0x01, 0x02, 0x04, 0xFF] {
            assert_eq!(
                decode_channel_meta(&raw(v, 0, b"x", b"")),
                Err(ChannelMetaError::UnknownVersion(v))
            );
        }
    }

    #[test]
    fn non_utf8_fields_are_refused() {
        assert_eq!(
            decode_channel_meta(&raw(3, 0, &[0xFF, 0xFE], b"")),
            Err(ChannelMetaError::Name(ChannelMetaFieldError::NotUtf8))
        );
        assert_eq!(
            decode_channel_meta(&raw(3, 0, b"x", &[0xC3, 0x28])),
            Err(ChannelMetaError::Description(
                ChannelMetaFieldError::NotUtf8
            ))
        );
    }

    #[test]
    fn an_out_of_bounds_field_rejects_the_whole_record() {
        assert_eq!(
            decode_channel_meta(&raw(3, 0, "Lakko\u{202E}".as_bytes(), b"fine")),
            Err(ChannelMetaError::Name(
                ChannelMetaFieldError::ControlOrFormatCharacter
            ))
        );
        assert_eq!(
            decode_channel_meta(&raw(3, 0, b"Fine", "bad\u{202E}".as_bytes())),
            Err(ChannelMetaError::Description(
                ChannelMetaFieldError::ControlOrFormatCharacter
            ))
        );
    }

    #[test]
    fn decode_refuses_what_compose_refuses() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            (" Lakko", ""),
            ("Lakko ", ""),
            ("La\nkko", ""),
            ("La\rkko", ""),
            ("La\u{2028}kko", ""),
            ("La\u{200B}kko", ""),
            ("La\u{AD}kko", ""),
            ("La\u{0}kko", ""),
            ("x", " padded"),
            ("x", "trailing\n"),
            ("x", "cr\rlf"),
            ("x", "ls\u{2028}ps"),
            ("x", "rlo\u{202E}"),
            ("x", "bom\u{FEFF}"),
            ("x", "esc\u{1B}"),
        ];
        for &(name, description) in cases {
            let compose_err = compose_channel_meta(&meta(name, description, 0)).unwrap_err();
            let decode_err =
                decode_channel_meta(&raw(3, 0, name.as_bytes(), description.as_bytes()))
                    .unwrap_err();
            assert_eq!(compose_err, decode_err, "{name:?} / {description:?}");
        }
        let long_name = "a".repeat(NAME_MAX_CHARS + 1);
        assert_eq!(
            compose_channel_meta(&meta(&long_name, "", 0)).unwrap_err(),
            decode_channel_meta(&raw(3, 0, long_name.as_bytes(), b"")).unwrap_err()
        );
        let long_description = "a".repeat(DESCRIPTION_MAX_CHARS + 1);
        assert_eq!(
            compose_channel_meta(&meta("x", &long_description, 0)).unwrap_err(),
            decode_channel_meta(&raw(3, 0, b"x", long_description.as_bytes())).unwrap_err()
        );
    }

    // --- name bounds ---------------------------------------------------------

    #[test]
    fn name_length_boundaries_discriminate() {
        assert_eq!(check_name(&"a".repeat(NAME_MAX_CHARS)), Ok(()));
        assert_eq!(
            check_name(&"a".repeat(NAME_MAX_CHARS + 1)),
            Err(ChannelMetaFieldError::TooLong { max_chars: 64 })
        );
        // 63 four-byte scalars = 252 bytes: fits. 64 = 256 bytes: one over
        // the one-byte length, refused although the char count is fine.
        assert_eq!(check_name(&"\u{1F600}".repeat(63)), Ok(()));
        assert_eq!(
            check_name(&"\u{1F600}".repeat(64)),
            Err(ChannelMetaFieldError::TooManyBytes { max_bytes: 255 })
        );
        assert_eq!(check_name(""), Err(ChannelMetaFieldError::Empty));
        assert_eq!(check_name("a"), Ok(()));
    }

    #[test]
    fn name_refuses_controls_formats_and_line_breaks() {
        use ChannelMetaFieldError as E;
        for (bad, why) in [
            ("La\u{0}kko", E::ControlOrFormatCharacter),
            ("La\u{1B}kko", E::ControlOrFormatCharacter),
            ("La\u{7F}kko", E::ControlOrFormatCharacter),
            ("La\u{202E}kko", E::ControlOrFormatCharacter), // RLO
            ("La\u{2066}kko", E::ControlOrFormatCharacter), // LRI
            ("La\u{200B}kko", E::ControlOrFormatCharacter), // ZWSP
            ("La\u{200E}kko", E::ControlOrFormatCharacter), // LRM
            ("La\u{2060}kko", E::ControlOrFormatCharacter), // WORD JOINER
            ("La\u{FEFF}kko", E::ControlOrFormatCharacter), // BOM
            ("La\u{AD}kko", E::ControlOrFormatCharacter),   // soft hyphen
            ("La\nkko", E::LineBreak),
            ("La\rkko", E::LineBreak),
            ("La\u{85}kko", E::LineBreak),
            ("La\u{2028}kko", E::LineBreak),
            ("La\u{2029}kko", E::LineBreak),
            (" Lakko", E::NotTrimmed),
            ("Lakko\t", E::NotTrimmed),
        ] {
            assert_eq!(check_name(bad), Err(why), "{bad:?}");
        }
    }

    #[test]
    fn name_accepts_ordinary_scripts_joiners_and_emoji() {
        for ok in [
            "Lakko News",
            "Новости",
            "李小龙",
            "محمد",
            "Zoë's Channel",
            "\u{1F600} Party",
            // ZWJ emoji sequence: man, ZWJ, woman, ZWJ, girl.
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} Family",
            // Persian spelling that needs ZWNJ.
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
        ] {
            assert_eq!(check_name(ok), Ok(()), "{ok:?}");
        }
    }

    // --- description bounds --------------------------------------------------

    #[test]
    fn description_length_boundaries_discriminate() {
        assert_eq!(check_description(""), Ok(()));
        assert_eq!(
            check_description(&"a".repeat(DESCRIPTION_MAX_CHARS)),
            Ok(())
        );
        assert_eq!(
            check_description(&"a".repeat(DESCRIPTION_MAX_CHARS + 1)),
            Err(ChannelMetaFieldError::TooLong { max_chars: 300 })
        );
        // 256 four-byte scalars = 1024 bytes: fits. 257 = 1028: refused
        // although the char count is well under 300.
        assert_eq!(check_description(&"\u{1F600}".repeat(256)), Ok(()));
        assert_eq!(
            check_description(&"\u{1F600}".repeat(257)),
            Err(ChannelMetaFieldError::TooManyBytes { max_bytes: 1024 })
        );
    }

    #[test]
    fn description_permits_line_feed_and_refuses_every_other_break() {
        use ChannelMetaFieldError as E;
        assert_eq!(check_description("one\ntwo\n\nfour"), Ok(()));
        for (bad, why) in [
            ("one\rtwo", E::LineBreak),
            ("one\r\ntwo", E::LineBreak),
            ("one\u{0B}two", E::LineBreak),
            ("one\u{0C}two", E::LineBreak),
            ("one\u{85}two", E::LineBreak),
            ("one\u{2028}two", E::LineBreak),
            ("one\u{2029}two", E::LineBreak),
            ("one\u{202E}two", E::ControlOrFormatCharacter),
            ("one\u{200B}two", E::ControlOrFormatCharacter),
            ("one\u{0}two", E::ControlOrFormatCharacter),
            ("one\u{1B}two", E::ControlOrFormatCharacter),
            ("\none", E::NotTrimmed),
            ("one\n", E::NotTrimmed),
            (" one", E::NotTrimmed),
        ] {
            assert_eq!(check_description(bad), Err(why), "{bad:?}");
        }
    }

    // --- classifier ----------------------------------------------------------

    #[test]
    fn a_non_record_is_parsed_as_the_chat_envelope() {
        let post = lakko_attachments::compose("hello channel", Vec::new()).unwrap();
        match classify_post_plaintext(&post) {
            PostContent::Message {
                plaintext,
                body: MessageBody::Chat { text, .. },
            } => {
                assert_eq!(plaintext, post.as_slice());
                assert_eq!(text, "hello channel");
            },
            other => panic!("expected a chat post, got {other:?}"),
        }
        let edit = lakko_attachments::compose_edit(7, "fixed").unwrap();
        assert!(matches!(
            classify_post_plaintext(&edit),
            PostContent::Message {
                body: MessageBody::Edit { target: 7, .. },
                ..
            }
        ));
        let delete = lakko_attachments::compose_delete(7).unwrap();
        assert!(matches!(
            classify_post_plaintext(&delete),
            PostContent::Message {
                body: MessageBody::Delete { target: 7 },
                ..
            }
        ));
        // Bytes that are not the record's domain plus NUL are a post, not a
        // record.
        for text in [
            &b"lakko/channel-meta/v2\x00"[..],
            b"lakko/channel-meta/v",
            b"lakko/channel-meta/v1 is the record domain",
        ] {
            assert!(matches!(
                classify_post_plaintext(text),
                PostContent::Message { .. }
            ));
        }
    }

    #[test]
    fn typing_receipts_and_chat_reactions_are_not_posts() {
        for bytes in [
            lakko_attachments::compose_typing(true).unwrap(),
            lakko_attachments::compose_read_receipt(3).unwrap(),
            lakko_attachments::compose_reaction(3, "\u{1F44D}").unwrap(),
        ] {
            assert_eq!(classify_post_plaintext(&bytes), PostContent::NotAPost);
        }
    }

    fn handle(public: bool, name: &str) -> ConversationHandle {
        ConversationHandle::sign(
            &[0x5E; 32],
            &[0x0C; crate::CHANNEL_ID_LEN],
            9,
            public,
            name,
            [0x7A; lakko_contacts::conversation_handle::CONVERSATION_HANDLE_CERT_LEN],
        )
        .unwrap()
    }

    #[test]
    fn a_handle_rides_the_record_and_its_flag_must_match() {
        let with = ChannelMeta {
            public: true,
            handle: Some(handle(true, "berlin_news")),
            ..meta("Berlin news", "", 5)
        };
        let wire = compose_channel_meta(&with).unwrap();
        let back = decode_channel_meta(&wire).unwrap();
        assert_eq!(back, with);
        let key = lakko_crypto::signing::SigningKey::from_seed(&[0x5E; 32])
            .verifying_key()
            .to_bytes();
        assert!(back
            .handle
            .unwrap()
            .verify_signature(&[0x0C; crate::CHANNEL_ID_LEN], &key)
            .is_ok());

        let mismatch = ChannelMeta {
            public: false,
            ..with
        };
        assert_eq!(
            compose_channel_meta(&mismatch),
            Err(ChannelMetaError::HandleFlagMismatch)
        );

        // A handle length outside the envelope bounds is refused before the
        // envelope is read.
        let mut short = compose_channel_meta(&meta("Lakko", "", 1)).unwrap();
        short.pop(); // the reactions mode
        let len_at = short.len() - 2;
        short[len_at..].copy_from_slice(&5u16.to_be_bytes());
        short.extend_from_slice(&[0; 5]);
        short.push(0);
        assert!(matches!(
            decode_channel_meta(&short),
            Err(ChannelMetaError::Handle(_))
        ));
    }

    #[test]
    fn classify_returns_malformed_for_a_matching_domain_out_of_bounds() {
        assert_eq!(
            classify_post_plaintext(&raw(3, 0, b"", b"")),
            PostContent::Malformed(ChannelMetaError::Name(ChannelMetaFieldError::Empty))
        );
        assert_eq!(
            classify_post_plaintext(&raw(1, 0, b"x", b"")),
            PostContent::Malformed(ChannelMetaError::UnknownVersion(1))
        );
        let mut trailing = raw(3, 0, b"x", b"");
        trailing.push(9);
        assert_eq!(
            classify_post_plaintext(&trailing),
            PostContent::Malformed(ChannelMetaError::TrailingBytes { count: 1 })
        );
    }

    // --- order ---------------------------------------------------------------

    #[test]
    fn newer_is_strictly_greater() {
        assert!(channel_meta_is_newer(2, 1));
        assert!(!channel_meta_is_newer(1, 1), "equal is not newer");
        assert!(!channel_meta_is_newer(0, 1));
        assert!(channel_meta_is_newer(u64::MAX, u64::MAX - 1));
        assert!(!channel_meta_is_newer(u64::MAX, u64::MAX));
    }

    // --- codes and hygiene ---------------------------------------------------

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(ChannelMetaError::WrongDomain.code(), "wrongDomain");
        assert_eq!(ChannelMetaError::Truncated.code(), "truncated");
        assert_eq!(
            ChannelMetaError::TrailingBytes { count: 3 }.code(),
            "trailingBytes"
        );
        assert_eq!(ChannelMetaError::UnknownVersion(9).code(), "unknownVersion");
        assert_eq!(
            ChannelMetaError::Name(ChannelMetaFieldError::Empty).code(),
            "name.empty"
        );
        assert_eq!(
            ChannelMetaError::Description(ChannelMetaFieldError::TooManyBytes { max_bytes: 1024 })
                .code(),
            "description.tooManyBytes"
        );
        assert_eq!(
            ChannelMetaFieldError::ControlOrFormatCharacter.code(),
            "controlOrFormatCharacter"
        );
    }

    #[test]
    fn error_messages_never_echo_the_text() {
        let err = check_name("Secret\u{202E}Name").unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        let err = decode_channel_meta(&raw(3, 0, "Secret\u{202E}".as_bytes(), b"")).unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        assert!(!format!("{err:?}").contains("Secret"));
    }

    #[test]
    fn refused_format_table_is_sorted_disjoint_and_hits_its_edges() {
        for w in REFUSED_FORMAT_CHAR_RANGES.windows(2) {
            assert!(w[0].1 < w[1].0, "{w:?}");
        }
        for &(lo, hi) in REFUSED_FORMAT_CHAR_RANGES {
            assert!(lo <= hi);
            for cp in [lo, hi] {
                let c = char::from_u32(cp).unwrap();
                assert!(is_refused_format_char(c), "{cp:#x}");
            }
        }
        assert!(!is_refused_format_char('\u{200C}'), "ZWNJ is permitted");
        assert!(!is_refused_format_char('\u{200D}'), "ZWJ is permitted");
        assert!(!is_refused_format_char('a'));
        assert!(!is_refused_format_char(' '));
    }

    #[test]
    fn classify_recognises_a_ban_list_before_the_text_path() {
        use crate::ban_list::{compose_ban_list, BanList, BanListError};
        let list = BanList {
            updated_at_ms: 4,
            authors: vec![[7; 32]],
        };
        let wire = compose_ban_list(&list).unwrap();
        assert_eq!(classify_post_plaintext(&wire), PostContent::BanList(list));
        let mut bad = wire;
        bad.push(0);
        assert_eq!(
            classify_post_plaintext(&bad),
            PostContent::MalformedBanList(BanListError::TrailingBytes)
        );
        // A bare separator is a malformed list, never a chat message.
        assert_eq!(
            classify_post_plaintext(crate::ban_list::BAN_LIST_DOMAIN_V1),
            PostContent::MalformedBanList(BanListError::Truncated)
        );
    }

    #[test]
    fn allowed_reactions_ride_the_record_and_are_bounded() {
        let thumbs = "\u{1F44D}".to_string();
        let heart = "\u{2764}\u{FE0F}".to_string();
        for reactions in [
            AllowedReactions::All,
            AllowedReactions::Off,
            AllowedReactions::Only(vec![heart.clone(), thumbs.clone()]),
        ] {
            let m = ChannelMeta {
                reactions: reactions.clone(),
                ..meta("Lakko", "", 1)
            };
            let wire = compose_channel_meta(&m).unwrap();
            assert_eq!(decode_channel_meta(&wire).unwrap().reactions, reactions);
        }
        let only = AllowedReactions::Only(vec![heart.clone(), thumbs.clone()]);
        assert!(only.allows(&thumbs));
        assert!(!only.allows("\u{1F525}"));
        assert!(AllowedReactions::All.allows("\u{1F525}"));
        assert!(!AllowedReactions::All.allows(""));
        assert!(!AllowedReactions::Off.allows(&thumbs));

        let bad = |r: AllowedReactions| {
            compose_channel_meta(&ChannelMeta {
                reactions: r,
                ..meta("Lakko", "", 1)
            })
        };
        assert_eq!(
            bad(AllowedReactions::Only(vec![])),
            Err(ChannelMetaError::Reactions)
        );
        assert_eq!(
            bad(AllowedReactions::Only(vec![thumbs.clone(), thumbs.clone()])),
            Err(ChannelMetaError::Reactions)
        );
        assert_eq!(
            bad(AllowedReactions::Only(vec![String::new()])),
            Err(ChannelMetaError::Reactions)
        );
        assert_eq!(
            bad(AllowedReactions::Only(vec![thumbs.clone(); 101])),
            Err(ChannelMetaError::Reactions)
        );

        let mut wire = compose_channel_meta(&meta("Lakko", "", 1)).unwrap();
        let last = wire.len() - 1;
        wire[last] = 3;
        assert_eq!(
            decode_channel_meta(&wire),
            Err(ChannelMetaError::ReactionsMode(3))
        );
        // A list count past the bound is refused before anything is read.
        wire[last] = 2;
        wire.push(101);
        assert_eq!(decode_channel_meta(&wire), Err(ChannelMetaError::Reactions));
        // A duplicate on the wire is refused by the shared validation.
        let dup = {
            let mut w = compose_channel_meta(&meta("Lakko", "", 1)).unwrap();
            w.pop();
            w.extend_from_slice(&[2, 2, 4]);
            w.extend_from_slice(thumbs.as_bytes());
            w.push(4);
            w.extend_from_slice(thumbs.as_bytes());
            w
        };
        assert_eq!(decode_channel_meta(&dup), Err(ChannelMetaError::Reactions));
    }
}
