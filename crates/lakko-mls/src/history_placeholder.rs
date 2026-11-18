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
//! version:           u8  == HISTORY_PLACEHOLDER_VERSION (4)
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
//!     title_present:  u8 (0 | 1)  [ + title_len: u16 + title UTF-8 ]
//!     artist_present: u8 (0 | 1)  [ + artist_len: u16 + artist UTF-8 ]
//! content_present:   u8 (0 | 1)
//!   [ content_kind:  u8 (1..=5)
//!     payload_len:   u32 (big-endian, 1..=MAX_CONTENT_PAYLOAD_BYTES) + payload
//!     state_len:     u16 (big-endian, 0..=MAX_CONTENT_STATE_BYTES) + state ]
//!   thumb:
//!     blob_id: 32 bytes, key: 32 bytes, nonce: 12 bytes, size: u64
//!     mime_present:  u8 (0 | 1)  [ + mime_len: u16 + mime UTF-8 ]
//!     width_present:  u8 (0 | 1)  [ + width: u32 ]
//!     height_present: u8 (0 | 1)  [ + height: u32 ]
//! ```
//!
//! ## Version 2: media metadata (ADR-0047)
//!
//! Version 1 carried no media fields, so a voice message, video note or
//! GIF restored from a history bundle came back as a plain file. Version 2
//! adds the ADR-0047 attachment fields (`kind`, `duration_ms`, `waveform`,
//! `width`, `height`, `thumb`). Nothing is deployed, so there is no
//! version-1 reader: the decoder refuses it.
//!
//! ## Version 3: audio tags (ADR-0090 section 2)
//!
//! Version 3 adds the `title` and `artist` tags of an `audio` attachment
//! after the thumb, so a music file restored from a history bundle keeps
//! its title line. Flag day: versions 1 and 2 are refused.
//!
//! ## Version 4: content messages (ADR-0090 section 3.11)
//!
//! A poll, checklist, location, contact card or article is not text, so
//! versions up to 3 restored it as an empty row. Version 4 carries, after
//! the attachments, the content kind, the content payload (the JSON value
//! of the envelope's content field, exactly what the envelope carried, the
//! latest edit for an edited checklist or article) and, for a poll or a
//! checklist, a snapshot of its folded state as the sending device saw it
//! (empty when none). All three are opaque here and bounded by length only;
//! the FFI decodes them with `lakko_attachments::Content::from_payload` and
//! `lakko_attachments::content_state::ContentState::decode`, the decoders
//! the envelope route uses. Flag day: version 3 is refused.
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
pub const HISTORY_PLACEHOLDER_VERSION: u8 = 4;

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

/// Hard cap on an audio attachment's `title` or `artist`, in UTF-8 bytes,
/// matching ADR-0090's `MAX_AUDIO_TAG_BYTES`.
pub const MAX_AUDIO_TAG_BYTES: usize = 255;

/// Largest content payload, bytes, matching
/// `lakko_attachments::content_state::MAX_CONTENT_PAYLOAD_BYTES` (chosen).
pub const MAX_CONTENT_PAYLOAD_BYTES: usize = 196_608;

/// Largest content state snapshot, bytes, matching
/// `lakko_attachments::content_state::MAX_CONTENT_STATE_BYTES` (chosen).
pub const MAX_CONTENT_STATE_BYTES: usize = 8_192;

/// Content kind bytes this codec carries (1 poll, 2 checklist, 3 location,
/// 4 contact card, 5 article).
pub const CONTENT_KINDS: core::ops::RangeInclusive<u8> = 1..=5;

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
    /// ADR-0090 audio title tag, at most [`MAX_AUDIO_TAG_BYTES`].
    pub title: Option<String>,
    /// ADR-0090 audio artist tag, at most [`MAX_AUDIO_TAG_BYTES`].
    pub artist: Option<String>,
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

/// An ADR-0090 content message inside a placeholder. Opaque here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryContentPlaceholder {
    /// Content kind byte, in [`CONTENT_KINDS`].
    pub kind: u8,
    /// The content payload, 1..=[`MAX_CONTENT_PAYLOAD_BYTES`] bytes.
    pub payload: Vec<u8>,
    /// The folded-state snapshot, 0..=[`MAX_CONTENT_STATE_BYTES`] bytes;
    /// empty when none.
    pub state: Vec<u8>,
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
    /// ADR-0090 content, for a poll, checklist, location, contact card or
    /// article message.
    pub content: Option<HistoryContentPlaceholder>,
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
            encode_opt_string(&mut out, att.title.as_deref(), MAX_AUDIO_TAG_BYTES, "title")?;
            encode_opt_string(
                &mut out,
                att.artist.as_deref(),
                MAX_AUDIO_TAG_BYTES,
                "artist",
            )?;
        }
        encode_content(&mut out, self.content.as_ref())?;
        Ok(out)
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
            let title = r.opt_string(MAX_AUDIO_TAG_BYTES, "title")?;
            let artist = r.opt_string(MAX_AUDIO_TAG_BYTES, "artist")?;
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
                title,
                artist,
            });
        }

        let content = decode_content(&mut r)?;

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
            content,
        })
    }
}

/// Encode the optional ADR-0090 content block.
fn encode_content(out: &mut Vec<u8>, content: Option<&HistoryContentPlaceholder>) -> Result<()> {
    match content {
        None => out.push(0),
        Some(content) => {
            if !CONTENT_KINDS.contains(&content.kind)
                || content.payload.is_empty()
                || content.payload.len() > MAX_CONTENT_PAYLOAD_BYTES
                || content.state.len() > MAX_CONTENT_STATE_BYTES
            {
                return Err(Error::Codec(
                    "history placeholder: content out of bounds".into(),
                ));
            }
            out.push(1);
            out.push(content.kind);
            let payload_len = u32::try_from(content.payload.len())
                .map_err(|_| Error::Codec("history placeholder: payload length overflow".into()))?;
            out.extend_from_slice(&payload_len.to_be_bytes());
            out.extend_from_slice(&content.payload);
            let state_len = u16::try_from(content.state.len())
                .map_err(|_| Error::Codec("history placeholder: state length overflow".into()))?;
            out.extend_from_slice(&state_len.to_be_bytes());
            out.extend_from_slice(&content.state);
        },
    }
    Ok(())
}

/// Decode the optional ADR-0090 content block.
fn decode_content(r: &mut Reader<'_>) -> Result<Option<HistoryContentPlaceholder>> {
    let content = if r.presence("content")? {
        let kind = r.u8()?;
        if !CONTENT_KINDS.contains(&kind) {
            return Err(Error::Codec(format!(
                "history placeholder: unknown content kind {kind}"
            )));
        }
        let payload_len = r.u32("payload_len")? as usize;
        if payload_len == 0 || payload_len > MAX_CONTENT_PAYLOAD_BYTES {
            return Err(Error::Codec(format!(
                "history placeholder: payload length {payload_len} not in 1..={MAX_CONTENT_PAYLOAD_BYTES}"
            )));
        }
        let payload = r.take(payload_len, "payload")?.to_vec();
        let state_len = r.u16()? as usize;
        if state_len > MAX_CONTENT_STATE_BYTES {
            return Err(Error::Codec(format!(
                "history placeholder: state length {state_len} exceeds cap {MAX_CONTENT_STATE_BYTES}"
            )));
        }
        let state = r.take(state_len, "state")?.to_vec();
        Some(HistoryContentPlaceholder {
            kind,
            payload,
            state,
        })
    } else {
        None
    };
    Ok(content)
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
            title: None,
            artist: None,
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
            title: None,
            artist: None,
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
            title: None,
            artist: None,
        }
    }

    fn sample() -> HistoryMessagePlaceholder {
        HistoryMessagePlaceholder {
            message_id: 0x0102_0304_0506_0708,
            direction: HistoryDirection::Incoming,
            created_at: "2026-06-04T12:00:00Z".into(),
            text: "hello \u{1f600} world".into(),
            attachments: vec![sample_attachment()],
            content: None,
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
            content: None,
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
            title: None,
            artist: None,
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
    fn versions_1_to_3_are_refused() {
        for old in [1u8, 2, 3] {
            let mut bytes = sample().encode().expect("encode");
            bytes[0] = old;
            let err = HistoryMessagePlaceholder::decode(&bytes).expect_err("old version refused");
            assert!(matches!(err, Error::Codec(m) if m.contains("version")));
        }
    }

    #[test]
    fn content_round_trips_and_is_bounded() {
        let mut msg = sample();
        msg.text = String::new();
        msg.attachments = vec![];
        msg.content = Some(HistoryContentPlaceholder {
            kind: 1,
            payload: b"{\"body\":\"x\"}".to_vec(),
            state: vec![1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        });
        let bytes = msg.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            msg
        );

        for bad in [
            HistoryContentPlaceholder {
                kind: 0,
                payload: vec![1],
                state: vec![],
            },
            HistoryContentPlaceholder {
                kind: 6,
                payload: vec![1],
                state: vec![],
            },
            HistoryContentPlaceholder {
                kind: 3,
                payload: vec![],
                state: vec![],
            },
            HistoryContentPlaceholder {
                kind: 3,
                payload: vec![1; MAX_CONTENT_PAYLOAD_BYTES + 1],
                state: vec![],
            },
            HistoryContentPlaceholder {
                kind: 2,
                payload: vec![1],
                state: vec![0; MAX_CONTENT_STATE_BYTES + 1],
            },
        ] {
            msg.content = Some(bad);
            assert!(msg.encode().is_err());
        }

        // A hand-made unknown kind byte is refused on decode.
        msg.content = Some(HistoryContentPlaceholder {
            kind: 5,
            payload: vec![7],
            state: vec![],
        });
        let mut bytes = msg.encode().expect("encode");
        // content_present, kind, payload_len(4), payload(1), state_len(2)
        let kind_at = bytes.len() - (1 + 4 + 1 + 2);
        bytes[kind_at] = 9;
        assert!(HistoryMessagePlaceholder::decode(&bytes).is_err());
    }

    #[test]
    fn audio_tags_round_trip_and_are_capped() {
        let mut att = voice_note_attachment();
        att.kind = Some("audio".into());
        att.waveform = None;
        att.title = Some("Song \u{1f3b5}".into());
        att.artist = Some("Band".into());
        let mut msg = sample();
        msg.attachments = vec![att.clone()];
        let bytes = msg.encode().expect("encode");
        let back = HistoryMessagePlaceholder::decode(&bytes).expect("decode");
        assert_eq!(back, msg);
        assert_eq!(back.attachments[0].title.as_deref(), Some("Song \u{1f3b5}"));

        let mut long = att;
        long.artist = Some("a".repeat(MAX_AUDIO_TAG_BYTES + 1));
        msg.attachments = vec![long];
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
        // From the end: content, artist, title, thumb, height and width presence
        // bytes, the 3 waveform bytes, then its u16 length.
        let waveform_len_at = bytes.len() - (1 + 1 + 1 + 1 + 1 + 1 + 3 + 2);
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
        // In a voice attachment without a thumb, title or artist, the thumb
        // presence byte is fourth from the end (then title, artist, content).
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
                (
                    opt_string(MAX_AUDIO_TAG_BYTES),
                    opt_string(MAX_AUDIO_TAG_BYTES),
                ),
            )
                .prop_map(
                    |(
                        (blob_id, key, nonce, size),
                        (mime, name, kind),
                        (duration_ms, waveform, width, height, thumb),
                        (title, artist),
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
                            title,
                            artist,
                        }
                    },
                )
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
                content in proptest::option::of((
                    1u8..=5,
                    proptest::collection::vec(any::<u8>(), 1..64),
                    proptest::collection::vec(any::<u8>(), 0..64),
                )),
            ) {
                let msg = HistoryMessagePlaceholder {
                    message_id,
                    direction,
                    created_at: created_at.unwrap_or_default(),
                    text: text.unwrap_or_default(),
                    attachments,
                    content: content.map(|(kind, payload, state)| HistoryContentPlaceholder {
                        kind,
                        payload,
                        state,
                    }),
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
            content: None,
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
            content: None,
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
