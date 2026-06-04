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
//! version:           u8  == HISTORY_PLACEHOLDER_VERSION (1)
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
//! ```
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
pub const HISTORY_PLACEHOLDER_VERSION: u8 = 1;

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
        }
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
            attachments.push(HistoryAttachmentPlaceholder {
                blob_id,
                key,
                nonce,
                size,
                mime,
                name,
            });
        }

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
        })
    }
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

    /// Read an optional string encoded as `present:u8 [+ u16 len + utf8]`.
    fn opt_string(&mut self, cap: usize, field: &str) -> Result<Option<String>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string_u16(cap, field)?)),
            other => Err(Error::Codec(format!(
                "history placeholder: {field} presence byte must be 0 or 1, got {other}"
            ))),
        }
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
        }
    }

    fn sample() -> HistoryMessagePlaceholder {
        HistoryMessagePlaceholder {
            message_id: 0x0102_0304_0506_0708,
            direction: HistoryDirection::Incoming,
            created_at: "2026-06-04T12:00:00Z".into(),
            text: "hello \u{1f600} world".into(),
            attachments: vec![sample_attachment()],
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
        }];
        let bytes = msg.encode().expect("encode");
        assert_eq!(
            HistoryMessagePlaceholder::decode(&bytes).expect("decode"),
            msg
        );
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
