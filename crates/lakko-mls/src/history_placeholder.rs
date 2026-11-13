// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Canonical, cross-platform codec for the history-bundle
//! per-message **placeholder** that lives inside each
//! `HistoryMessage.payload_bytes` (G-6).
//!
//! ## Why this exists
//!
//! The multi-device history catch-up bundle
//! ([`crate::history_bundle`]) ships each conversation message
//! as an opaque `payload_bytes` blob. The display projection a
//! client needs to reconstruct a row — `(message_id, direction,
//! text, created_at, attachments)` — was historically encoded
//! per-platform: web emitted `JSON.stringify` of its TypeScript
//! `PersistedMessage`, iOS a `JSONEncoder` of a Swift struct,
//! Android a `kotlinx.serialization` blob. Those three shapes
//! did **not** agree on the wire:
//!
//! * the placeholder `messageId` was a JSON **string** on web
//!   (the TS field is `string`) but a JSON **number** on the
//!   native platforms (`Int64` / `Long`), so a native
//!   `JSONDecoder` threw on web's bytes and silently dropped
//!   the message;
//! * the sibling `HistoryMessage.message_id` field was UTF-8 of
//!   a decimal string on web vs an 8-byte big-endian `u64` on
//!   native.
//!
//! Net effect: history catch-up interoperated between iOS and
//! Android but **not** across a web ⇄ native device pairing
//! (tracked as G-6 / G-9 in `docs/security/known-gaps.md`).
//!
//! ## The fix
//!
//! One codec in the shared Rust core, exposed verbatim to every
//! platform through the FFI crates, so all three encode and
//! decode the **same bytes**. The wire format is a small
//! deterministic binary layout (no JSON field-ordering
//! ambiguity, no floating-point, no map iteration order):
//!
//! ```text
//! version:           u8  == HISTORY_PLACEHOLDER_VERSION (3)
//! message_id:        u64 (big-endian)
//! direction:         u8  (0 = outgoing, 1 = incoming, 2 = system)
//! created_at_len:    u16 (big-endian)   + created_at UTF-8 bytes
//! text_len:          u32 (big-endian)   + text UTF-8 bytes
//! attachment_count:  u16 (big-endian)
//!   per attachment:
//!     blob_id:       32 bytes
//!     key:           32 bytes
//!     nonce:         12 bytes
//!     size:          u64 (big-endian)
//!     mime_present:  u8 (0 | 1)  [ + mime_len: u16 + mime UTF-8 ]
//!     name_present:  u8 (0 | 1)  [ + name_len: u16 + name UTF-8 ]
//!     kind_present:  u8 (0 | 1)  [ + kind_len: u16 + kind UTF-8 ]
//!     duration_present: u8 (0 | 1)  [ + duration_ms: u32 ]
//!     waveform_present: u8 (0 | 1)  [ + waveform_len: u16 + bytes ]
//!     width_present:  u8 (0 | 1)  [ + width: u32 ]
//!     height_present: u8 (0 | 1)  [ + height: u32 ]
//!     thumb_present:  u8 (0 | 1)  [ + thumb ]
//!   thumb:
//!     blob_id: 32 bytes, key: 32 bytes, nonce: 12 bytes, size: u64
//!     mime_present:  u8 (0 | 1)  [ + mime_len: u16 + mime UTF-8 ]
//!     width_present:  u8 (0 | 1)  [ + width: u32 ]
//!     height_present: u8 (0 | 1)  [ + height: u32 ]
//! card_present:      u8 (0 | 1)  [ + card_len: u16 + card bytes ]
//! reply_present:     u8 (0 | 1)  [ + reply ]
//!   reply:
//!     target:        i64 (big-endian)
//!     author_present:  u8 (0 | 1)  [ + author_len: u16 + author UTF-8 ]
//!     excerpt_present: u8 (0 | 1)  [ + excerpt_len: u16 + excerpt UTF-8 ]
//! forward_present:   u8 (0 | 1)  [ + forward ]
//!   forward:
//!     author_present:  u8 (0 | 1)  [ + author_len: u16 + author UTF-8 ]
//!     origin_present:  u8 (0 | 1)  [ + origin ]
//!   origin:
//!     group_len: u8 + group id bytes, message_id: i64 (big-endian)
//! ```
//!
//! ## Version 3: the channel invitation card, the reply, the forward
//!
//! Version 2 carried the text and the attachments only, so a message that
//! was a channel invitation (ADR-0087 section 14) reached a new device as
//! its bare `/g/` link, and a reply or a forward lost its quote or its
//! "Forwarded from" line. Version 3 appends the card bytes (opaque here,
//! 1 to [`MAX_CHANNEL_INVITE_BYTES`]), the reply (target `messageId`, the
//! author and excerpt snapshot) and the forward (author snapshot, the
//! origin group id and `messageId`). Version 2 is refused (flag day).
//!
//! ## Version 2: media metadata (ADR-0047)
//!
//! Version 1 carried no media fields, so a voice message, video note or
//! GIF restored from a history bundle came back as a plain file. Version 2
//! adds the ADR-0047 attachment fields (`kind`, `duration_ms`, `waveform`,
//! `width`, `height`, `thumb`). Nothing is deployed, so there is no
//! version-1 reader: the decoder refuses it.
//!
//! This codec enforces **structure** only (lengths, caps, presence bytes).
//! The **semantic** ADR-0047 bounds (a duration only for a voice or video
//! note and within its limit, a dimension within 1..=4096, a thumb within
//! its size limit) live in `lakko-attachments`, which `lakko-mls` does not
//! depend on; the FFI crates apply them on both encode and decode so every
//! platform restores exactly what a live message would have shown.
//!
//! [`HistoryMessagePlaceholder::decode`] is a bounded parser:
//! every length is checked against the remaining slice and
//! against a hard cap, integers are read big-endian, and
//! trailing bytes are rejected. It is suitable as a fuzz
//! target (attacker-touchable: a malicious history bundle
//! reaches this parser after the AEAD open succeeds, e.g. a
//! compromised peer device on the same account).
//!
//! `created_at` is carried as the platform's own display string
//! (ISO-8601 today) rather than a numeric timestamp so the
//! round-trip is lossless and no platform has to reformat;
//! `message_id` / `size` are carried as the typed `u64` so each
//! platform renders them in its native representation (web's
//! decimal string, native's integer).

use crate::error::{Error, Result};

/// Wire-format version byte. Bump on any layout change; the
/// decoder refuses any other value so an older binary fails
/// loudly rather than mis-parsing a newer placeholder.
pub const HISTORY_PLACEHOLDER_VERSION: u8 = 3;

/// Hard cap on the message text, in UTF-8 bytes (1 MiB). Larger
/// inputs are refused at encode time and rejected at decode.
pub const MAX_TEXT_BYTES: usize = 1 << 20;

/// Hard cap on the per-message attachment count.
pub const MAX_ATTACHMENTS: usize = 64;

/// Hard cap on the `created_at` string, in UTF-8 bytes. ISO-8601
/// timestamps are ~24 bytes; 256 is generous slack.
pub const MAX_CREATED_AT_BYTES: usize = 256;

/// Hard cap on an attachment `mime` string, in UTF-8 bytes.
pub const MAX_MIME_BYTES: usize = 256;

/// Hard cap on an attachment `name` string, in UTF-8 bytes.
pub const MAX_NAME_BYTES: usize = 1024;

/// Hard cap on an attachment media `kind` string, in UTF-8 bytes. The
/// known kinds (`voice`, `video_note`, `gif`) are far shorter; an unknown
/// kind is carried verbatim within this cap.
pub const MAX_KIND_BYTES: usize = 32;

/// Hard cap on the waveform, in bytes (one amplitude per bar), matching
/// ADR-0047's `MAX_WAVEFORM_BYTES`.
pub const MAX_WAVEFORM_BYTES: usize = 128;

/// Hard cap on a channel invitation card (ADR-0087 section 14), in bytes.
/// Equal to `lakko_attachments::MAX_CHANNEL_INVITE_BYTES`, which `lakko-mls`
/// does not depend on; the FFI crates pin the two equal in a test.
pub const MAX_CHANNEL_INVITE_BYTES: usize = 2048;

/// Hard cap on a reply's or a forward's author snapshot, in UTF-8 bytes.
/// Equal to `lakko_attachments::MAX_REPLY_AUTHOR_BYTES` and
/// `MAX_FORWARDED_FROM_BYTES` (pinned in the FFI crates).
pub const MAX_SNAPSHOT_AUTHOR_BYTES: usize = 255;

/// Hard cap on a reply's excerpt snapshot, in UTF-8 bytes. Equal to
/// `lakko_attachments::MAX_REPLY_EXCERPT_BYTES` (pinned in the FFI crates).
pub const MAX_REPLY_EXCERPT_BYTES: usize = 512;

/// Hard cap on a forward's origin group id, in bytes: the decoded size of
/// the envelope's `MAX_FORWARD_ORIGIN_GROUP_BYTES` (128) base64 characters.
pub const MAX_ORIGIN_GROUP_BYTES: usize = 96;

/// SHA-256 blob id length.
const BLOB_ID_LEN: usize = 32;
/// Content-encryption key length.
const KEY_LEN: usize = 32;
/// AEAD nonce length.
const NONCE_LEN: usize = 12;

/// Message direction relative to the local account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    /// Sent by this account.
    Outgoing,
    /// Received from a peer.
    Incoming,
    /// System / control message (e.g. membership change).
    System,
}

impl HistoryDirection {
    /// Wire tag for the direction.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Outgoing => 0,
            Self::Incoming => 1,
            Self::System => 2,
        }
    }

    /// Parse a direction wire tag.
    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            0 => Ok(Self::Outgoing),
            1 => Ok(Self::Incoming),
            2 => Ok(Self::System),
            other => Err(Error::Codec(format!(
                "history placeholder: unknown direction tag {other}"
            ))),
        }
    }
}

/// One attachment reference inside a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryAttachmentPlaceholder {
    /// 32-byte SHA-256 blob id.
    pub blob_id: [u8; BLOB_ID_LEN],
    /// 32-byte content-encryption key.
    pub key: [u8; KEY_LEN],
    /// 12-byte AEAD nonce.
    pub nonce: [u8; NONCE_LEN],
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional MIME type.
    pub mime: Option<String>,
    /// Optional file name.
    pub name: Option<String>,
    /// ADR-0047 media kind (`voice`, `video_note`, `gif`, or unknown).
    pub kind: Option<String>,
    /// ADR-0047 playback length in milliseconds.
    pub duration_ms: Option<u32>,
    /// ADR-0047 waveform amplitudes, at most [`MAX_WAVEFORM_BYTES`].
    pub waveform: Option<Vec<u8>>,
    /// ADR-0047 pixel width.
    pub width: Option<u32>,
    /// ADR-0047 pixel height.
    pub height: Option<u32>,
    /// ADR-0047 poster-frame blob.
    pub thumb: Option<HistoryThumbPlaceholder>,
}

/// A poster-frame blob attached to a [`HistoryAttachmentPlaceholder`]
/// (ADR-0047).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryThumbPlaceholder {
    /// 32-byte SHA-256 blob id.
    pub blob_id: [u8; BLOB_ID_LEN],
    /// 32-byte content-encryption key.
    pub key: [u8; KEY_LEN],
    /// 12-byte AEAD nonce.
    pub nonce: [u8; NONCE_LEN],
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional MIME type.
    pub mime: Option<String>,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
}

/// The reply a [`HistoryMessagePlaceholder`] carries (ADR-0052): the target's
/// DS `messageId` and the snapshot the replier saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryReplyPlaceholder {
    /// The target's DS `messageId`.
    pub target: i64,
    /// The quoted author's name as the replier saw it.
    pub author: Option<String>,
    /// The quoted excerpt as the replier saw it.
    pub excerpt: Option<String>,
}

/// The forward a [`HistoryMessagePlaceholder`] carries (ADR-0052 decision 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryForwardPlaceholder {
    /// The original author's name, a snapshot.
    pub author: Option<String>,
    /// The origin conversation and message, kept only as a whole.
    pub origin: Option<HistoryForwardOrigin>,
}

/// Where a forwarded message came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryForwardOrigin {
    /// The origin conversation's group id, 1 to [`MAX_ORIGIN_GROUP_BYTES`].
    pub group_id: Vec<u8>,
    /// The original's DS `messageId`.
    pub message_id: i64,
}

/// The decoded display projection of one history-bundle message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMessagePlaceholder {
    /// HLC-derived message id (rendered as a decimal string on
    /// web, an integer on native).
    pub message_id: u64,
    /// Direction relative to the local account.
    pub direction: HistoryDirection,
    /// Display timestamp string as the platform stores it
    /// (ISO-8601 today). Carried verbatim for a lossless
    /// round-trip.
    pub created_at: String,
    /// Message text.
    pub text: String,
    /// Attachment references.
    pub attachments: Vec<HistoryAttachmentPlaceholder>,
    /// ADR-0087 section 14 — the channel invitation card the message's
    /// envelope carried, opaque here, 1 to [`MAX_CHANNEL_INVITE_BYTES`].
    pub channel_invite: Option<Vec<u8>>,
    /// ADR-0052 — the reply this message is.
    pub reply: Option<HistoryReplyPlaceholder>,
    /// ADR-0052 decision 2 — the forward attribution this message carries.
    pub forward: Option<HistoryForwardPlaceholder>,
}

impl HistoryMessagePlaceholder {
    /// Serialize to the canonical wire bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] when any field exceeds its hard
    /// cap (`text` > [`MAX_TEXT_BYTES`], attachment count >
    /// [`MAX_ATTACHMENTS`], `created_at` / `mime` / `name` over
    /// their caps). Refusing at encode time guarantees every
    /// emitted blob round-trips through [`Self::decode`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        let created_at = self.created_at.as_bytes();
        if created_at.len() > MAX_CREATED_AT_BYTES {
            return Err(Error::Codec(format!(
                "history placeholder: created_at {} bytes exceeds cap {MAX_CREATED_AT_BYTES}",
                created_at.len()
            )));
        }
        let text = self.text.as_bytes();
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::Codec(format!(
                "history placeholder: text {} bytes exceeds cap {MAX_TEXT_BYTES}",
                text.len()
            )));
        }
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(Error::Codec(format!(
                "history placeholder: {} attachments exceeds cap {MAX_ATTACHMENTS}",
                self.attachments.len()
            )));
        }

        let mut out = Vec::new();
        out.push(HISTORY_PLACEHOLDER_VERSION);
        out.extend_from_slice(&self.message_id.to_be_bytes());
        out.push(self.direction.tag());

        // created_at: u16 len + bytes (len fits — checked above).
        let created_len = u16::try_from(created_at.len())
            .map_err(|_| Error::Codec("history placeholder: created_at length overflow".into()))?;
        out.extend_from_slice(&created_len.to_be_bytes());
        out.extend_from_slice(created_at);

        // text: u32 len + bytes (len fits — checked above).
        let text_len = u32::try_from(text.len())
            .map_err(|_| Error::Codec("history placeholder: text length overflow".into()))?;
        out.extend_from_slice(&text_len.to_be_bytes());
        out.extend_from_slice(text);

        // attachments: u16 count, then each entry.
        let att_count = u16::try_from(self.attachments.len())
            .map_err(|_| Error::Codec("history placeholder: attachment count overflow".into()))?;
        out.extend_from_slice(&att_count.to_be_bytes());
        for att in &self.attachments {
            out.extend_from_slice(&att.blob_id);
            out.extend_from_slice(&att.key);
            out.extend_from_slice(&att.nonce);
            out.extend_from_slice(&att.size.to_be_bytes());
            encode_opt_string(&mut out, att.mime.as_deref(), MAX_MIME_BYTES, "mime")?;
            encode_opt_string(&mut out, att.name.as_deref(), MAX_NAME_BYTES, "name")?;
            encode_opt_string(&mut out, att.kind.as_deref(), MAX_KIND_BYTES, "kind")?;
            encode_opt_u32(&mut out, att.duration_ms);
            encode_opt_bytes(
                &mut out,
                att.waveform.as_deref(),
                MAX_WAVEFORM_BYTES,
                "waveform",
            )?;
            encode_opt_u32(&mut out, att.width);
            encode_opt_u32(&mut out, att.height);
            match &att.thumb {
                None => out.push(0),
                Some(thumb) => {
                    out.push(1);
                    out.extend_from_slice(&thumb.blob_id);
                    out.extend_from_slice(&thumb.key);
                    out.extend_from_slice(&thumb.nonce);
                    out.extend_from_slice(&thumb.size.to_be_bytes());
                    encode_opt_string(
                        &mut out,
                        thumb.mime.as_deref(),
                        MAX_MIME_BYTES,
                        "thumb mime",
                    )?;
                    encode_opt_u32(&mut out, thumb.width);
                    encode_opt_u32(&mut out, thumb.height);
                },
            }
        }

        self.encode_extras(&mut out)?;
        Ok(out)
    }

    /// Append the version-3 card, reply and forward.
    fn encode_extras(&self, out: &mut Vec<u8>) -> Result<()> {
        match &self.channel_invite {
            None => out.push(0),
            Some(card) => {
                if card.is_empty() || card.len() > MAX_CHANNEL_INVITE_BYTES {
                    return Err(Error::Codec(format!(
                        "history placeholder: channel invite {} bytes not in 1..={MAX_CHANNEL_INVITE_BYTES}",
                        card.len()
                    )));
                }
                encode_opt_bytes(out, Some(card), MAX_CHANNEL_INVITE_BYTES, "channel invite")?;
            },
        }
        match &self.reply {
            None => out.push(0),
            Some(reply) => {
                out.push(1);
                out.extend_from_slice(&reply.target.to_be_bytes());
                encode_opt_string(
                    out,
                    reply.author.as_deref(),
                    MAX_SNAPSHOT_AUTHOR_BYTES,
                    "reply author",
                )?;
                encode_opt_string(
                    out,
                    reply.excerpt.as_deref(),
                    MAX_REPLY_EXCERPT_BYTES,
                    "reply excerpt",
                )?;
            },
        }
        match &self.forward {
            None => out.push(0),
            Some(forward) => {
                out.push(1);
                encode_opt_string(
                    out,
                    forward.author.as_deref(),
                    MAX_SNAPSHOT_AUTHOR_BYTES,
                    "forward author",
                )?;
                match &forward.origin {
                    None => out.push(0),
                    Some(origin) => {
                        let len = u8::try_from(origin.group_id.len())
                            .ok()
                            .filter(|n| *n != 0 && usize::from(*n) <= MAX_ORIGIN_GROUP_BYTES)
                            .ok_or_else(|| {
                                Error::Codec(format!(
                                    "history placeholder: forward origin group {} bytes not in 1..={MAX_ORIGIN_GROUP_BYTES}",
                                    origin.group_id.len()
                                ))
                            })?;
                        out.push(1);
                        out.push(len);
                        out.extend_from_slice(&origin.group_id);
                        out.extend_from_slice(&origin.message_id.to_be_bytes());
                    },
                }
            },
        }
        Ok(())
    }

    /// Parse the canonical wire bytes. Bounded: every length is
    /// checked against the remaining slice and against a hard
    /// cap, and trailing bytes are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] on a wrong version byte, a
    /// truncated buffer, a length over its cap, a non-UTF-8
    /// string field, an unknown direction tag, or trailing
    /// bytes after the last attachment.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != HISTORY_PLACEHOLDER_VERSION {
            return Err(Error::Codec(format!(
                "history placeholder: unsupported version {version}"
            )));
        }
        let message_id = r.u64()?;
        let direction = HistoryDirection::from_tag(r.u8()?)?;
        let created_at = r.string_u16(MAX_CREATED_AT_BYTES, "created_at")?;
        let text = r.string_u32(MAX_TEXT_BYTES, "text")?;

        let att_count = r.u16()? as usize;
        if att_count > MAX_ATTACHMENTS {
            return Err(Error::Codec(format!(
                "history placeholder: attachment count {att_count} exceeds cap {MAX_ATTACHMENTS}"
            )));
        }
        let mut attachments = Vec::with_capacity(att_count);
        for _ in 0..att_count {
            let blob_id = r.array::<BLOB_ID_LEN>("blob_id")?;
            let key = r.array::<KEY_LEN>("key")?;
            let nonce = r.array::<NONCE_LEN>("nonce")?;
            let size = r.u64()?;
            let mime = r.opt_string(MAX_MIME_BYTES, "mime")?;
            let name = r.opt_string(MAX_NAME_BYTES, "name")?;
            let kind = r.opt_string(MAX_KIND_BYTES, "kind")?;
            let duration_ms = r.opt_u32("duration_ms")?;
            let waveform = r.opt_bytes(MAX_WAVEFORM_BYTES, "waveform")?;
            let width = r.opt_u32("width")?;
            let height = r.opt_u32("height")?;
            let thumb = if r.presence("thumb")? {
                Some(HistoryThumbPlaceholder {
                    blob_id: r.array::<BLOB_ID_LEN>("thumb blob_id")?,
                    key: r.array::<KEY_LEN>("thumb key")?,
                    nonce: r.array::<NONCE_LEN>("thumb nonce")?,
                    size: r.u64()?,
                    mime: r.opt_string(MAX_MIME_BYTES, "thumb mime")?,
                    width: r.opt_u32("thumb width")?,
                    height: r.opt_u32("thumb height")?,
                })
            } else {
                None
            };
            attachments.push(HistoryAttachmentPlaceholder {
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
            });
        }

        let (channel_invite, reply, forward) = decode_extras(&mut r)?;

        if !r.is_empty() {
            return Err(Error::Codec(format!(
                "history placeholder: {} trailing byte(s)",
                r.remaining()
            )));
        }

        Ok(Self {
            message_id,
            direction,
            created_at,
            text,
            attachments,
            channel_invite,
            reply,
            forward,
        })
    }
}

/// The version-3 card, reply and forward, read after the attachments.
type Extras = (
    Option<Vec<u8>>,
    Option<HistoryReplyPlaceholder>,
    Option<HistoryForwardPlaceholder>,
);

/// Read the version-3 card, reply and forward.
fn decode_extras(r: &mut Reader<'_>) -> Result<Extras> {
    let channel_invite = r.opt_bytes(MAX_CHANNEL_INVITE_BYTES, "channel invite")?;
    if channel_invite.as_ref().is_some_and(Vec::is_empty) {
        return Err(Error::Codec(
            "history placeholder: channel invite is empty".into(),
        ));
    }
    let reply = if r.presence("reply")? {
        Some(HistoryReplyPlaceholder {
            target: r.i64("reply target")?,
            author: r.opt_string(MAX_SNAPSHOT_AUTHOR_BYTES, "reply author")?,
            excerpt: r.opt_string(MAX_REPLY_EXCERPT_BYTES, "reply excerpt")?,
        })
    } else {
        None
    };
    let forward = if r.presence("forward")? {
        let author = r.opt_string(MAX_SNAPSHOT_AUTHOR_BYTES, "forward author")?;
        let origin = if r.presence("forward origin")? {
            let len = usize::from(r.u8()?);
            if len == 0 || len > MAX_ORIGIN_GROUP_BYTES {
                return Err(Error::Codec(format!(
                    "history placeholder: forward origin group length {len} not in 1..={MAX_ORIGIN_GROUP_BYTES}"
                )));
            }
            Some(HistoryForwardOrigin {
                group_id: r.take(len, "forward origin group")?.to_vec(),
                message_id: r.i64("forward origin message")?,
            })
        } else {
            None
        };
        Some(HistoryForwardPlaceholder { author, origin })
    } else {
        None
    };
    Ok((channel_invite, reply, forward))
}

/// Encode an optional string as `present:u8 [+ len:u16 + utf8]`.
fn encode_opt_string(out: &mut Vec<u8>, s: Option<&str>, cap: usize, field: &str) -> Result<()> {
    match s {
        None => out.push(0),
        Some(value) => {
            let bytes = value.as_bytes();
            if bytes.len() > cap {
                return Err(Error::Codec(format!(
                    "history placeholder: {field} {} bytes exceeds cap {cap}",
                    bytes.len()
                )));
            }
            let len = u16::try_from(bytes.len()).map_err(|_| {
                Error::Codec(format!("history placeholder: {field} length overflow"))
            })?;
            out.push(1);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(bytes);
        },
    }
    Ok(())
}

/// Encode an optional `u32` as `present:u8 [+ u32 BE]`.
fn encode_opt_u32(out: &mut Vec<u8>, v: Option<u32>) {
    match v {
        None => out.push(0),
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        },
    }
}

/// Encode optional bytes as `present:u8 [+ len:u16 + bytes]`.
fn encode_opt_bytes(out: &mut Vec<u8>, b: Option<&[u8]>, cap: usize, field: &str) -> Result<()> {
    match b {
        None => out.push(0),
        Some(bytes) => {
            if bytes.len() > cap {
                return Err(Error::Codec(format!(
                    "history placeholder: {field} {} bytes exceeds cap {cap}",
                    bytes.len()
                )));
            }
            let len = u16::try_from(bytes.len()).map_err(|_| {
                Error::Codec(format!("history placeholder: {field} length overflow"))
            })?;
            out.push(1);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(bytes);
        },
    }
    Ok(())
}

/// Bounded big-endian reader over a borrowed slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize, field: &str) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::Codec(format!(
                "history placeholder: truncated reading {field} (need {n}, have {})",
                self.remaining()
            )));
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1, "u8")?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8, "u64")?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(b);
        Ok(u64::from_be_bytes(arr))
    }

    fn i64(&mut self, field: &str) -> Result<i64> {
        Ok(i64::from_be_bytes(self.array::<8>(field)?))
    }

    fn array<const N: usize>(&mut self, field: &str) -> Result<[u8; N]> {
        let b = self.take(N, field)?;
        let mut arr = [0u8; N];
        arr.copy_from_slice(b);
        Ok(arr)
    }

    /// Read a `u16`-length-prefixed UTF-8 string with a cap.
    fn string_u16(&mut self, cap: usize, field: &str) -> Result<String> {
        let len = self.u16()? as usize;
        self.string_body(len, cap, field)
    }

    /// Read a `u32`-length-prefixed UTF-8 string with a cap.
    fn string_u32(&mut self, cap: usize, field: &str) -> Result<String> {
        let b = self.take(4, field)?;
        let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        self.string_body(len, cap, field)
    }

    fn string_body(&mut self, len: usize, cap: usize, field: &str) -> Result<String> {
        if len > cap {
            return Err(Error::Codec(format!(
                "history placeholder: {field} length {len} exceeds cap {cap}"
            )));
        }
        let bytes = self.take(len, field)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::Codec(format!("history placeholder: {field} is not valid UTF-8")))
    }

    /// Read a presence byte: `0` absent, `1` present, anything else refused.
    fn presence(&mut self, field: &str) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::Codec(format!(
                "history placeholder: {field} presence byte must be 0 or 1, got {other}"
            ))),
        }
    }

    /// Read an optional string encoded as `present:u8 [+ u16 len + utf8]`.
    fn opt_string(&mut self, cap: usize, field: &str) -> Result<Option<String>> {
        if self.presence(field)? {
            Ok(Some(self.string_u16(cap, field)?))
        } else {
            Ok(None)
        }
    }

    fn u32(&mut self, field: &str) -> Result<u32> {
        let b = self.take(4, field)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read an optional `u32` encoded as `present:u8 [+ u32 BE]`.
    fn opt_u32(&mut self, field: &str) -> Result<Option<u32>> {
        if self.presence(field)? {
            Ok(Some(self.u32(field)?))
        } else {
            Ok(None)
        }
    }

    /// Read optional bytes encoded as `present:u8 [+ u16 len + bytes]`.
    fn opt_bytes(&mut self, cap: usize, field: &str) -> Result<Option<Vec<u8>>> {
        if !self.presence(field)? {
            return Ok(None);
        }
        let len = self.u16()? as usize;
        if len > cap {
            return Err(Error::Codec(format!(
                "history placeholder: {field} length {len} exceeds cap {cap}"
            )));
        }
        Ok(Some(self.take(len, field)?.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_attachment() -> HistoryAttachmentPlaceholder {
        HistoryAttachmentPlaceholder {
            blob_id: [0x11; 32],
            key: [0x22; 32],
            nonce: [0x33; 12],
            size: 4096,
            mime: Some("image/png".into()),
            name: Some("photo.png".into()),
            kind: None,
            duration_ms: None,
            waveform: None,
            width: Some(640),
            height: Some(480),
            thumb: None,
        }
    }

    fn voice_note_attachment() -> HistoryAttachmentPlaceholder {
        HistoryAttachmentPlaceholder {
            blob_id: [0x44; 32],
            key: [0x55; 32],
            nonce: [0x66; 12],
            size: 12_345,
            mime: Some("audio/mp4".into()),
            name: Some("voice-1.m4a".into()),
            kind: Some("voice".into()),
            duration_ms: Some(7_250),
            waveform: Some((0u8..=127).collect()),
            width: None,
            height: None,
            thumb: None,
        }
    }

    fn video_note_attachment() -> HistoryAttachmentPlaceholder {
        HistoryAttachmentPlaceholder {
            blob_id: [0x77; 32],
            key: [0x88; 32],
            nonce: [0x99; 12],
            size: 800_000,
            mime: Some("video/mp4".into()),
            name: Some("video-note.mp4".into()),
            kind: Some("video_note".into()),
            duration_ms: Some(42_000),
            waveform: None,
            width: Some(480),
            height: Some(480),
            thumb: Some(HistoryThumbPlaceholder {
                blob_id: [0xAA; 32],
                key: [0xBB; 32],
                nonce: [0xCC; 12],
                size: 20_000,
                mime: Some("image/jpeg".into()),
                width: Some(240),
                height: None,
            }),
        }
    }

    fn sample() -> HistoryMessagePlaceholder {
        HistoryMessagePlaceholder {
            message_id: 0x0102_0304_0506_0708,
            direction: HistoryDirection::Incoming,
            created_at: "2026-06-04T12:00:00Z".into(),
            text: "hello \u{1f600} world".into(),
            attachments: vec![sample_attachment()],
            channel_invite: None,
            reply: None,
            forward: None,
        }
    }

    #[test]
    fn round_trip_full() {
        let msg = sample();
        let bytes = msg.encode().expect("encode");
        let back = HistoryMessagePlaceholder::decode(&bytes).expect("decode");
        assert_eq!(msg, back);
    }

    #[test]
    fn round_trip_no_attachments_no_optionals() {
        let msg = HistoryMessagePlaceholder {
            message_id: 0,
            direction: HistoryDirection::System,
            created_at: String::new(),
            text: String::new(),
            attachments: vec![],
            channel_invite: None,
            reply: None,
            forward: None,
        };
        let bytes = msg.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            msg
        );
    }

    #[test]
    fn round_trip_attachment_with_none_optionals() {
        let mut msg = sample();
        msg.attachments = vec![HistoryAttachmentPlaceholder {
            blob_id: [0xAB; 32],
            key: [0xCD; 32],
            nonce: [0xEF; 12],
            size: u64::MAX,
            mime: None,
            name: None,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
        }];
        let bytes = msg.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            msg
        );
    }

    #[test]
    fn media_attachments_round_trip() {
        let mut msg = sample();
        msg.attachments = vec![
            voice_note_attachment(),
            video_note_attachment(),
            sample_attachment(),
        ];
        let bytes = msg.encode().expect("encode");
        let back = HistoryMessagePlaceholder::decode(&bytes).expect("decode");
        assert_eq!(back, msg);
        assert_eq!(back.attachments[0].kind.as_deref(), Some("voice"));
        assert_eq!(
            back.attachments[1].thumb.as_ref().map(|t| t.size),
            Some(20_000)
        );
    }

    #[test]
    fn version_1_is_refused() {
        let mut bytes = sample().encode().expect("encode");
        bytes[0] = 1;
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("v1 must be refused");
        assert!(matches!(err, Error::Codec(m) if m.contains("version")));
    }

    #[test]
    fn version_2_is_refused() {
        let mut bytes = sample().encode().expect("encode");
        bytes[0] = 2;
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("v2 must be refused");
        assert!(matches!(err, Error::Codec(m) if m.contains("version")));
    }

    fn with_card_reply_forward() -> HistoryMessagePlaceholder {
        let mut msg = sample();
        msg.channel_invite = Some(vec![0x5A; 300]);
        msg.reply = Some(HistoryReplyPlaceholder {
            target: -7,
            author: Some("Ann".into()),
            excerpt: Some("see you \u{1f600}".into()),
        });
        msg.forward = Some(HistoryForwardPlaceholder {
            author: Some("Bob".into()),
            origin: Some(HistoryForwardOrigin {
                group_id: vec![0x42; 32],
                message_id: 1234,
            }),
        });
        msg
    }

    #[test]
    fn card_reply_and_forward_round_trip() {
        let msg = with_card_reply_forward();
        let bytes = msg.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            msg
        );

        let mut bare = msg;
        bare.reply = Some(HistoryReplyPlaceholder {
            target: 0,
            author: None,
            excerpt: None,
        });
        bare.forward = Some(HistoryForwardPlaceholder {
            author: None,
            origin: None,
        });
        let bytes = bare.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            bare
        );
    }

    #[test]
    fn card_bounds_are_enforced_both_ways() {
        let mut msg = sample();
        msg.channel_invite = Some(vec![1; MAX_CHANNEL_INVITE_BYTES]);
        let at_bound = msg.encode().expect("a card at the bound encodes");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&at_bound).expect("decode"),
            msg
        );

        msg.channel_invite = Some(vec![1; MAX_CHANNEL_INVITE_BYTES + 1]);
        assert!(msg.encode().is_err());
        msg.channel_invite = Some(Vec::new());
        assert!(msg.encode().is_err());

        // A declared card length over the cap, or an empty present card, is
        // refused on decode.
        let mut bytes = sample().encode().expect("encode");
        let card_presence_at = bytes.len() - 3;
        bytes.truncate(card_presence_at);
        let mut over = bytes.clone();
        over.push(1);
        over.extend_from_slice(
            &u16::try_from(MAX_CHANNEL_INVITE_BYTES + 1)
                .expect("fits")
                .to_be_bytes(),
        );
        over.extend(std::iter::repeat_n(0u8, MAX_CHANNEL_INVITE_BYTES + 1));
        over.extend_from_slice(&[0, 0]);
        assert!(HistoryMessagePlaceholder::decode(&over).is_err());
        let mut empty = bytes;
        empty.extend_from_slice(&[1, 0, 0, 0, 0]);
        assert!(HistoryMessagePlaceholder::decode(&empty).is_err());
    }

    #[test]
    fn forward_origin_group_bounds_are_enforced_both_ways() {
        let mut msg = with_card_reply_forward();
        let origin = |len: usize| HistoryForwardOrigin {
            group_id: vec![9; len],
            message_id: 1,
        };
        msg.forward = Some(HistoryForwardPlaceholder {
            author: None,
            origin: Some(origin(MAX_ORIGIN_GROUP_BYTES)),
        });
        assert!(msg.encode().is_ok());
        msg.forward = Some(HistoryForwardPlaceholder {
            author: None,
            origin: Some(origin(MAX_ORIGIN_GROUP_BYTES + 1)),
        });
        assert!(msg.encode().is_err());
        msg.forward = Some(HistoryForwardPlaceholder {
            author: None,
            origin: Some(origin(0)),
        });
        assert!(msg.encode().is_err());

        // Hand-built: forward present, no author, origin present with a zero
        // group length.
        let mut bytes = sample().encode().expect("encode");
        bytes.pop();
        bytes.extend_from_slice(&[1, 0, 1, 0]);
        bytes.extend_from_slice(&1i64.to_be_bytes());
        assert!(HistoryMessagePlaceholder::decode(&bytes).is_err());
    }

    #[test]
    fn oversized_reply_and_forward_snapshots_are_refused() {
        let mut msg = sample();
        msg.reply = Some(HistoryReplyPlaceholder {
            target: 1,
            author: Some("a".repeat(MAX_SNAPSHOT_AUTHOR_BYTES + 1)),
            excerpt: None,
        });
        assert!(msg.encode().is_err());
        msg.reply = Some(HistoryReplyPlaceholder {
            target: 1,
            author: None,
            excerpt: Some("e".repeat(MAX_REPLY_EXCERPT_BYTES + 1)),
        });
        assert!(msg.encode().is_err());
        msg.reply = None;
        msg.forward = Some(HistoryForwardPlaceholder {
            author: Some("f".repeat(MAX_SNAPSHOT_AUTHOR_BYTES + 1)),
            origin: None,
        });
        assert!(msg.encode().is_err());
    }

    #[test]
    fn oversized_waveform_and_kind_are_refused_both_ways() {
        let mut msg = sample();
        let mut att = voice_note_attachment();
        att.waveform = Some(vec![7; MAX_WAVEFORM_BYTES + 1]);
        msg.attachments = vec![att];
        assert!(msg.encode().is_err());

        let mut msg = sample();
        let mut att = voice_note_attachment();
        att.kind = Some("k".repeat(MAX_KIND_BYTES + 1));
        msg.attachments = vec![att];
        assert!(msg.encode().is_err());

        // A hand-edited waveform length over the cap is refused on decode.
        let mut msg = sample();
        let mut att = sample_attachment();
        att.waveform = Some(vec![1; 3]);
        att.width = None;
        att.height = None;
        msg.attachments = vec![att];
        let mut bytes = msg.encode().expect("encode");
        // After the waveform: width, height, thumb presence, then the
        // card, reply and forward presence bytes (all absent).
        let waveform_len_at = bytes.len() - (1 + 1 + 1 + 3 + 3 + 2);
        bytes[waveform_len_at..waveform_len_at + 2].copy_from_slice(
            &u16::try_from(MAX_WAVEFORM_BYTES + 1)
                .expect("fits")
                .to_be_bytes(),
        );
        assert!(HistoryMessagePlaceholder::decode(&bytes).is_err());
    }

    #[test]
    fn media_presence_byte_other_than_0_or_1_is_refused() {
        let mut msg = sample();
        msg.attachments = vec![voice_note_attachment()];
        let bytes = msg.encode().expect("encode");
        // The last byte of a voice attachment without a thumb is the thumb
        // presence byte; the card, reply and forward presence bytes follow.
        let mut bad = bytes;
        let last = bad.len() - 4;
        assert_eq!(bad[last], 0);
        bad[last] = 2;
        let err = HistoryMessagePlaceholder::decode(&bad).expect_err("must reject");
        assert!(matches!(err, Error::Codec(m) if m.contains("thumb") && m.contains("presence")));
    }

    mod prop {
        use super::super::*;
        use proptest::prelude::*;

        fn opt_string(max: usize) -> impl Strategy<Value = Option<String>> {
            proptest::option::of(
                proptest::string::string_regex(&format!(".{{0,{max}}}"))
                    .expect("regex")
                    .prop_filter("within byte cap", move |s| s.len() <= max),
            )
        }

        fn thumb() -> impl Strategy<Value = HistoryThumbPlaceholder> {
            (
                any::<[u8; 32]>(),
                any::<[u8; 32]>(),
                any::<[u8; 12]>(),
                any::<u64>(),
                opt_string(40),
                any::<Option<u32>>(),
                any::<Option<u32>>(),
            )
                .prop_map(|(blob_id, key, nonce, size, mime, width, height)| {
                    HistoryThumbPlaceholder {
                        blob_id,
                        key,
                        nonce,
                        size,
                        mime,
                        width,
                        height,
                    }
                })
        }

        fn attachment() -> impl Strategy<Value = HistoryAttachmentPlaceholder> {
            (
                (
                    any::<[u8; 32]>(),
                    any::<[u8; 32]>(),
                    any::<[u8; 12]>(),
                    any::<u64>(),
                ),
                (opt_string(40), opt_string(40), opt_string(MAX_KIND_BYTES)),
                (
                    any::<Option<u32>>(),
                    proptest::option::of(proptest::collection::vec(
                        any::<u8>(),
                        0..=MAX_WAVEFORM_BYTES,
                    )),
                    any::<Option<u32>>(),
                    any::<Option<u32>>(),
                    proptest::option::of(thumb()),
                ),
            )
                .prop_map(
                    |(
                        (blob_id, key, nonce, size),
                        (mime, name, kind),
                        (duration_ms, waveform, width, height, thumb),
                    )| {
                        HistoryAttachmentPlaceholder {
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
                        }
                    },
                )
        }

        fn reply() -> impl Strategy<Value = HistoryReplyPlaceholder> {
            (
                any::<i64>(),
                opt_string(MAX_SNAPSHOT_AUTHOR_BYTES),
                opt_string(MAX_REPLY_EXCERPT_BYTES),
            )
                .prop_map(|(target, author, excerpt)| HistoryReplyPlaceholder {
                    target,
                    author,
                    excerpt,
                })
        }

        fn forward() -> impl Strategy<Value = HistoryForwardPlaceholder> {
            (
                opt_string(MAX_SNAPSHOT_AUTHOR_BYTES),
                proptest::option::of((
                    proptest::collection::vec(any::<u8>(), 1..=MAX_ORIGIN_GROUP_BYTES),
                    any::<i64>(),
                )),
            )
                .prop_map(|(author, origin)| HistoryForwardPlaceholder {
                    author,
                    origin: origin.map(|(group_id, message_id)| HistoryForwardOrigin {
                        group_id,
                        message_id,
                    }),
                })
        }

        proptest! {
            /// Any placeholder within the caps encodes, decodes to an equal
            /// value, and re-encodes to identical bytes.
            #[test]
            fn any_placeholder_round_trips_byte_stably(
                message_id in any::<u64>(),
                direction in prop_oneof![
                    Just(HistoryDirection::Outgoing),
                    Just(HistoryDirection::Incoming),
                    Just(HistoryDirection::System),
                ],
                created_at in opt_string(40),
                text in opt_string(200),
                attachments in proptest::collection::vec(attachment(), 0..4),
                channel_invite in proptest::option::of(
                    proptest::collection::vec(any::<u8>(), 1..=MAX_CHANNEL_INVITE_BYTES),
                ),
                reply in proptest::option::of(reply()),
                forward in proptest::option::of(forward()),
            ) {
                let msg = HistoryMessagePlaceholder {
                    message_id,
                    direction,
                    created_at: created_at.unwrap_or_default(),
                    text: text.unwrap_or_default(),
                    attachments,
                    channel_invite,
                    reply,
                    forward,
                };
                let bytes = msg.encode().expect("within caps");
                let back = HistoryMessagePlaceholder::decode(&bytes).expect("decodes");
                prop_assert_eq!(&back, &msg);
                prop_assert_eq!(back.encode().expect("re-encodes"), bytes);
            }
        }
    }

    #[test]
    fn all_directions_round_trip() {
        for dir in [
            HistoryDirection::Outgoing,
            HistoryDirection::Incoming,
            HistoryDirection::System,
        ] {
            let mut msg = sample();
            msg.direction = dir;
            let bytes = msg.encode().expect("encode");
            assert_eq!(
                HistoryMessagePlaceholder::decode(&bytes)
                    .expect("decode")
                    .direction,
                dir
            );
        }
    }

    #[test]
    fn encode_is_deterministic() {
        // Same logical message encodes to identical bytes every
        // time — the cross-platform determinism guarantee.
        let msg = sample();
        assert_eq!(msg.encode().expect("a"), msg.encode().expect("b"));
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let mut bytes = sample().encode().expect("encode");
        bytes[0] = 0xFF;
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("must reject");
        match err {
            Error::Codec(m) => assert!(m.contains("version"), "msg: {m}"),
            other => panic!("expected Codec, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let mut bytes = sample().encode().expect("encode");
        bytes.push(0x00);
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("must reject");
        match err {
            Error::Codec(m) => assert!(m.contains("trailing"), "msg: {m}"),
            other => panic!("expected Codec, got {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_truncated() {
        let bytes = sample().encode().expect("encode");
        for cut in 0..bytes.len() {
            // Any prefix shorter than the full message must error,
            // never panic.
            let _ = HistoryMessagePlaceholder::decode(&bytes[..cut]);
        }
        // Specifically, the empty buffer is truncated at the version.
        assert!(HistoryMessagePlaceholder::decode(&[]).is_err());
    }

    #[test]
    fn decode_rejects_bad_direction_tag() {
        let mut bytes = sample().encode().expect("encode");
        // version(1) + message_id(8) => direction tag at index 9.
        bytes[9] = 0x09;
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("must reject");
        match err {
            Error::Codec(m) => assert!(m.contains("direction"), "msg: {m}"),
            other => panic!("expected Codec, got {other:?}"),
        }
    }

    #[test]
    fn encode_rejects_oversized_text() {
        let msg = HistoryMessagePlaceholder {
            message_id: 1,
            direction: HistoryDirection::Outgoing,
            created_at: "t".into(),
            text: "a".repeat(MAX_TEXT_BYTES + 1),
            attachments: vec![],
            channel_invite: None,
            reply: None,
            forward: None,
        };
        assert!(msg.encode().is_err());
    }

    #[test]
    fn encode_rejects_too_many_attachments() {
        let msg = HistoryMessagePlaceholder {
            message_id: 1,
            direction: HistoryDirection::Outgoing,
            created_at: "t".into(),
            text: "x".into(),
            attachments: vec![sample_attachment(); MAX_ATTACHMENTS + 1],
            channel_invite: None,
            reply: None,
            forward: None,
        };
        assert!(msg.encode().is_err());
    }

    #[test]
    fn decode_rejects_oversized_declared_text_len() {
        // Hand-craft a header whose declared text length exceeds
        // the cap; the parser must refuse before allocating.
        let mut bytes = Vec::new();
        bytes.push(HISTORY_PLACEHOLDER_VERSION);
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.push(HistoryDirection::Outgoing.tag());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // created_at len 0
        bytes.extend_from_slice(&(u32::MAX).to_be_bytes()); // text len = 4 GiB
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("must reject");
        match err {
            Error::Codec(m) => assert!(m.contains("text") && m.contains("cap"), "msg: {m}"),
            other => panic!("expected Codec, got {other:?}"),
        }
    }

    #[test]
    fn non_utf8_string_rejected() {
        // Build a valid header with a created_at length of 1 but a
        // lone 0xFF continuation byte (invalid UTF-8).
        let mut bytes = Vec::new();
        bytes.push(HISTORY_PLACEHOLDER_VERSION);
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.push(HistoryDirection::Outgoing.tag());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.push(0xFF);
        // text len 0, attachment count 0
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("must reject");
        match err {
            Error::Codec(m) => assert!(m.contains("UTF-8"), "msg: {m}"),
            other => panic!("expected Codec, got {other:?}"),
        }
    }
}
