// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-attachments` surface — the decode/classify half only
//! (ADR-0041 Phase 2).
//!
//! Mirror of the read subset of `lakko-ffi-uniffi`'s `attachments` module:
//! [`parse_body_envelope`] + the [`ParsedBody`] / [`MessageBodyKind`] /
//! [`AttachmentRef`] shapes it returns. The compose / encrypt / decrypt-blob
//! functions are DELIBERATELY absent — the NSE only classifies an
//! already-decrypted MLS application body to render a preview; it never
//! composes an envelope or downloads a blob.

use lakko_attachments as inner;

/// The classification of a decoded message body (ADR-0023). Mirror of
/// [`lakko_attachments::MessageBody`] flattened for Swift: the `kind` selects
/// which of [`ParsedBody`]'s optional fields are meaningful.
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageBodyKind {
    /// A plain text + attachments message.
    Chat,
    /// Edit of `target` — `text` is the new body.
    Edit,
    /// Delete (tombstone) of `target`.
    Delete,
    /// Reaction `emoji` to `target`.
    Reaction,
    /// ADR-0024 — an ephemeral typing signal (`is_typing`).
    Typing,
    /// ADR-0024 — a read receipt: read up to `target` (inclusive).
    ReadReceipt,
    /// A Lakko service envelope this build did not recognise (a newer
    /// profile, group-name, pin or beacon version). Never render it; drop it
    /// like any other service envelope. Appended last so every earlier
    /// variant keeps its ordinal.
    ServiceEnvelope,
    /// ADR-0061 section 6 — a chat envelope from a newer wire version; the
    /// version rides `target` and `text` is empty. The extension renders its
    /// own catalog word for the notification, never core prose. Appended
    /// last so every earlier variant keeps its ordinal.
    Unsupported,
}

/// A decoded attachment reference (ADR-0023).
///
/// Flat record of raw bytes; the base64 the JSON wire format uses is decoded
/// inside [`parse_body_envelope`]. The NSE renders the `name` / `mime` / `size`
/// into a preview but does not fetch the blob.
#[derive(uniffi::Record, Clone)]
pub struct AttachmentRef {
    /// Raw 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// Raw 32-byte CEK.
    pub key: Vec<u8>,
    /// Raw 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Optional MIME type (e.g. `image/jpeg`).
    pub mime: Option<String>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional human-visible filename.
    pub name: Option<String>,
    /// ADR-0047 — `"voice"`, `"video_note"`, `"gif"`, another (unknown)
    /// value, or `None` for a plain file. Lets a preview say "Voice
    /// message" instead of a file name.
    #[uniffi(default = None)]
    pub kind: Option<String>,
    /// ADR-0047 — playback length in ms (voice / video note only, bounded
    /// by the core parser).
    #[uniffi(default = None)]
    pub duration_ms: Option<u32>,
    /// ADR-0047 — raw waveform amplitude bytes, 1..=128.
    #[uniffi(default = None)]
    pub waveform: Option<Vec<u8>>,
    /// ADR-0047 — pixel width, 1..=4096.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// ADR-0047 — pixel height, 1..=4096.
    #[uniffi(default = None)]
    pub height: Option<u32>,
    /// ADR-0047 — the poster-frame blob reference (decoded, never fetched
    /// here).
    #[uniffi(default = None)]
    pub thumb: Option<ThumbRef>,
}

/// ADR-0047 — a decoded poster-frame blob reference, the parse-side mirror
/// of the full surface's `ThumbRef`.
#[derive(uniffi::Record, Clone)]
pub struct ThumbRef {
    /// Raw 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// Raw 32-byte CEK.
    pub key: Vec<u8>,
    /// Raw 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Optional MIME type of the poster.
    pub mime: Option<String>,
    /// Plaintext size in bytes, 1..=256 KiB.
    pub size: u64,
    /// Pixel width, 1..=4096.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// Pixel height, 1..=4096.
    #[uniffi(default = None)]
    pub height: Option<u32>,
}

/// Output of [`parse_body_envelope`]: the classified message body
/// (ADR-0023/0024). `text` + `attachments` are meaningful for `Chat` (and
/// `text` for `Edit`); `target` for every op; `emoji` for `Reaction`.
#[derive(uniffi::Record)]
pub struct ParsedBody {
    /// Which kind of message this is.
    pub kind: MessageBodyKind,
    /// Chat / edit text (empty for delete / reaction).
    pub text: String,
    /// Attachment references (chat only; empty otherwise).
    pub attachments: Vec<AttachmentRef>,
    /// The DS `messageId` the op acts on (`None` for `Chat`/`Typing`;
    /// for `ReadReceipt` it is the read-up-to watermark; for `Unsupported`
    /// the envelope's wire version).
    pub target: Option<i64>,
    /// The reaction emoji (`Some` only for `Reaction`).
    pub emoji: Option<String>,
    /// ADR-0024 — the typing state (`Some` only for `Typing`).
    pub is_typing: Option<bool>,
}

/// Decode an MLS Application message body into a classified [`ParsedBody`]
/// (ADR-0023/0024). Never fails — a malformed / ambiguous op degrades to
/// `Chat`, the same fail-safe posture the full surface uses.
#[uniffi::export]
#[must_use]
pub fn parse_body_envelope(bytes: Vec<u8>) -> ParsedBody {
    match inner::parse_body(&bytes) {
        // ADR-0052: a reply's quote is not previewed by a notification, and
        // this surface stays the smallest that decrypts (C-59), so the reply
        // is matched away rather than carried. The same goes for the forward
        // attribution and the ADR-0065 re-send marker: a notification needs
        // neither, and deduplicating a re-sent row is the foreground app's
        // job.
        inner::MessageBody::Chat {
            text, attachments, ..
        } => ParsedBody {
            kind: MessageBodyKind::Chat,
            text,
            attachments: refs_to_ffi(attachments),
            target: None,
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Edit { target, text } => ParsedBody {
            kind: MessageBodyKind::Edit,
            text,
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Delete { target } => ParsedBody {
            kind: MessageBodyKind::Delete,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Reaction { target, emoji } => ParsedBody {
            kind: MessageBodyKind::Reaction,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: Some(emoji),
            is_typing: None,
        },
        inner::MessageBody::Typing { is_typing } => ParsedBody {
            kind: MessageBodyKind::Typing,
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: Some(is_typing),
        },
        inner::MessageBody::ReadReceipt { up_to } => ParsedBody {
            kind: MessageBodyKind::ReadReceipt,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(up_to),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Unsupported { version } => ParsedBody {
            kind: MessageBodyKind::Unsupported,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(i64::from(version)),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::ServiceEnvelope => ParsedBody {
            kind: MessageBodyKind::ServiceEnvelope,
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: None,
        },
    }
}

/// Decode inner `AttachmentRef`s into the FFI flat shape, dropping any
/// malformed ref silently (the same policy as the full surface).
fn refs_to_ffi(refs: Vec<inner::AttachmentRef>) -> Vec<AttachmentRef> {
    refs.into_iter()
        .filter_map(|r| {
            let blob_id = r.decode_blob_id().ok()?;
            let key = r.decode_key().ok()?;
            let nonce = r.decode_nonce().ok()?;
            let waveform = r.decode_waveform();
            let thumb = r.thumb.as_ref().and_then(|t| {
                Some(ThumbRef {
                    blob_id: t.decode_blob_id().ok()?.to_vec(),
                    key: t.decode_key().ok()?.as_bytes().to_vec(),
                    nonce: t.decode_nonce().ok()?.to_vec(),
                    mime: t.mime.clone(),
                    size: t.size,
                    width: t.width,
                    height: t.height,
                })
            });
            Some(AttachmentRef {
                blob_id: blob_id.to_vec(),
                key: key.as_bytes().to_vec(),
                nonce: nonce.to_vec(),
                mime: r.mime,
                size: r.size,
                name: r.name,
                kind: r.kind,
                duration_ms: r.duration_ms,
                waveform,
                width: r.width,
                height: r.height,
                thumb,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_metadata_reaches_the_extension_parse_shape() {
        let enc = inner::encrypt_attachment(b"voice").unwrap();
        let r = inner::AttachmentRef::from_encrypted(
            &enc,
            5,
            Some("audio/mp4".to_string()),
            Some("voice-1.m4a".to_string()),
        )
        .with_kind(inner::MEDIA_KIND_VOICE)
        .with_duration_ms(4200)
        .with_waveform(&[9, 8, 7]);
        let body = parse_body_envelope(inner::compose("", vec![r]).unwrap());
        assert_eq!(body.kind, MessageBodyKind::Chat);
        let got = &body.attachments[0];
        assert_eq!(got.kind.as_deref(), Some("voice"));
        assert_eq!(got.duration_ms, Some(4200));
        assert_eq!(got.waveform, Some(vec![9, 8, 7]));
        assert!(got.thumb.is_none());
    }

    #[test]
    fn newer_wire_version_is_unsupported_with_no_text() {
        let body = parse_body_envelope(br#"{"v":7,"text":"future"}"#.to_vec());
        assert_eq!(body.kind, MessageBodyKind::Unsupported);
        assert_eq!(body.target, Some(7));
        assert!(body.text.is_empty());
    }
}
