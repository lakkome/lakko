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
    /// for `ReadReceipt` it is the read-up-to watermark).
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
        inner::MessageBody::Chat { text, attachments } => ParsedBody {
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
            Some(AttachmentRef {
                blob_id: blob_id.to_vec(),
                key: key.as_bytes().to_vec(),
                nonce: nonce.to_vec(),
                mime: r.mime,
                size: r.size,
                name: r.name,
            })
        })
        .collect()
}
