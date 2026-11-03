// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-attachments` (Phase 1.19b).
//!
//! Mirror of the Rust crate's public surface, flattened for
//! Swift / Kotlin idioms:
//!
//! - `EncryptedAttachment` becomes a flat record of byte arrays
//!   (the inner `AeadKey` is zeroize-on-drop in pure Rust, but
//!   UniFFI doesn't preserve that property across the FFI; the
//!   caller is responsible for handling the bytes carefully).
//! - `AttachmentRef` becomes a flat record of byte arrays + an
//!   optional MIME + size + name. The base64 encoding that the
//!   inner crate uses for the JSON wire format is hidden behind
//!   `compose_envelope` / `parse_envelope` — callers think in
//!   raw bytes.
//! - `encrypt_attachment` / `decrypt_attachment` / `compose_envelope`
//!   / `parse_envelope` are top-level free functions, surfacing
//!   in Swift + Kotlin as free functions (matches the
//!   `composeContactRequest` + `powSolve` precedent).
//!
//! ## Why flat records instead of opaque handles
//!
//! `EncryptedAttachment` carries a fresh-random CEK that the
//! caller persists into an MLS Application message (via
//! `compose_envelope`). The natural shape for that flow is "give
//! me bytes I can put on the wire," not "give me a handle I have
//! to thread through a separate `to_bytes` call." Flat records
//! also avoid a UniFFI lifetime gotcha — `Object`-typed handles
//! get auto-cleaned-up only on garbage-collection, and we'd
//! rather have the CEK leave heap as soon as Swift/Kotlin drops
//! the `Data` / `ByteArray` reference.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attachments as inner;
use lakko_crypto::aead::AeadKey;

use crate::FfiError;

// ============================================================================
// FfiError integration
// ============================================================================

impl From<inner::AttachmentError> for FfiError {
    fn from(e: inner::AttachmentError) -> Self {
        FfiError::Attachment(e.to_string())
    }
}

// ============================================================================
// Records
// ============================================================================

/// Output of [`encrypt_attachment`]: opaque ciphertext + the
/// per-attachment secrets needed to decrypt it.
///
/// Swift / Kotlin callers PUT `ciphertext` to
/// `/blob/<hex(blob_id)>` on the storage service, then ride
/// `key` + `nonce` + `blob_id` (alongside MIME + size + name)
/// inside an [`AttachmentRef`] in the MLS Application message
/// composed via [`compose_envelope`].
#[derive(uniffi::Record, Clone)]
pub struct EncryptedAttachment {
    /// Ciphertext bytes — `ChaCha20-Poly1305(plaintext)` with AAD
    /// empty. Upload verbatim; the storage server hashes them on
    /// PUT to verify the URL's content-addressed `blob_id`.
    pub ciphertext: Vec<u8>,
    /// Fresh random 32-byte ChaCha20-Poly1305 key (CEK).
    pub key: Vec<u8>,
    /// Fresh random 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// `SHA-256(ciphertext)` — the storage service's content
    /// address. 32 bytes raw; caller hex-encodes for the URL path.
    pub blob_id: Vec<u8>,
}

/// One attachment reference inside an MLS Application message
/// envelope. Mirror of [`lakko_attachments::AttachmentRef`] with
/// the base64 encoding hidden (raw `Vec<u8>` over the FFI; the
/// JSON wire format is built inside [`compose_envelope`]).
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
    /// Plaintext size in bytes — sized progress affordances + a
    /// silent-truncation tripwire on the receive path.
    pub size: u64,
    /// Optional human-visible filename.
    pub name: Option<String>,
    /// ADR-0047 — `"voice"`, `"video_note"`, `"gif"`, another (unknown)
    /// value that renders as a file, or `None` for a plain file. Chooses
    /// the bubble only; inline playback is decided by [`inline_media_type`]
    /// on `mime`, never by this.
    #[uniffi(default = None)]
    pub kind: Option<String>,
    /// ADR-0047 — playback length in ms. Survives only on a `voice`
    /// (<= 15 min) or `video_note` (<= 60 s) ref; dropped otherwise.
    #[uniffi(default = None)]
    pub duration_ms: Option<u32>,
    /// ADR-0047 — raw amplitude bytes, 1..=128. Out of bound is dropped.
    #[uniffi(default = None)]
    pub waveform: Option<Vec<u8>>,
    /// ADR-0047 — pixel width, 1..=4096. Out of bound is dropped.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// ADR-0047 — pixel height, 1..=4096. Out of bound is dropped.
    #[uniffi(default = None)]
    pub height: Option<u32>,
    /// ADR-0047 — the poster-frame blob (video note still).
    #[uniffi(default = None)]
    pub thumb: Option<ThumbRef>,
}

/// ADR-0047 — a poster-frame blob attached to an [`AttachmentRef`]: its
/// own encrypted blob on the storage service, under its own CEK. Mirror
/// of [`lakko_attachments::ThumbRef`] with raw bytes over the FFI.
#[derive(uniffi::Record, Clone)]
pub struct ThumbRef {
    /// Raw 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// Raw 32-byte CEK.
    pub key: Vec<u8>,
    /// Raw 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Optional MIME type of the poster (check with [`inline_media_type`]).
    pub mime: Option<String>,
    /// Plaintext size in bytes, 1..=256 KiB; out of bound drops the thumb.
    pub size: u64,
    /// Pixel width, 1..=4096. Out of bound is dropped.
    #[uniffi(default = None)]
    pub width: Option<u32>,
    /// Pixel height, 1..=4096. Out of bound is dropped.
    #[uniffi(default = None)]
    pub height: Option<u32>,
}

/// ADR-0047 — how an attachment may be rendered inline. Mirror of
/// [`lakko_attachments::InlineMediaType`].
#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum InlineMediaType {
    /// Render as an image.
    Image,
    /// Play as audio.
    Audio,
    /// Play as video.
    Video,
}

impl From<inner::InlineMediaType> for InlineMediaType {
    fn from(t: inner::InlineMediaType) -> Self {
        match t {
            inner::InlineMediaType::Image => Self::Image,
            inner::InlineMediaType::Audio => Self::Audio,
            inner::InlineMediaType::Video => Self::Video,
        }
    }
}

/// Output of [`parse_envelope`]: the user-visible text + zero or
/// more [`AttachmentRef`]s. Attachments[] is always present,
/// possibly empty — Swift / Kotlin idioms prefer empty arrays over
/// nullable arrays.
#[derive(uniffi::Record)]
pub struct ParsedEnvelope {
    pub text: String,
    pub attachments: Vec<AttachmentRef>,
}

// ============================================================================
// Free functions
// ============================================================================

/// Encrypt `plaintext` under a fresh random CEK + nonce. Returns
/// the ciphertext + secrets + `blob_id`. The CEK rides in the MLS
/// Application message via [`compose_envelope`] — never crosses
/// the storage-service wire.
///
/// # Errors
///
/// - [`FfiError::Attachment`] (wraps
///   [`lakko_attachments::AttachmentError::TooLarge`]) when
///   `plaintext.len()` exceeds `MAX_ATTACHMENT_BYTES` (50 MB).
/// - [`FfiError::Attachment`] (wraps
///   [`lakko_attachments::AttachmentError::Crypto`]) on a library-
///   level AEAD failure (effectively impossible for valid inputs).
#[uniffi::export]
pub fn encrypt_attachment(plaintext: Vec<u8>) -> Result<EncryptedAttachment, FfiError> {
    let enc = inner::encrypt_attachment(&plaintext)?;
    Ok(EncryptedAttachment {
        ciphertext: enc.ciphertext,
        // `as_bytes()` is the canonical accessor on `AeadKey`; the
        // wrapper itself zeroizes-on-drop, but the Vec we hand
        // back at the FFI doesn't — see the module doc above for
        // why we accept the trade-off.
        key: enc.key.as_bytes().to_vec(),
        nonce: enc.nonce.to_vec(),
        blob_id: enc.blob_id.to_vec(),
    })
}

/// Decrypt `ciphertext` under `key` + `nonce`. Returns the plaintext.
///
/// # Errors
///
/// - [`FfiError::Attachment`] on wrong-length `key` or `nonce`.
/// - [`FfiError::Attachment`] on AEAD tag mismatch (wrong key /
///   nonce / tampered ciphertext).
#[uniffi::export]
pub fn decrypt_attachment(
    ciphertext: Vec<u8>,
    key: Vec<u8>,
    nonce: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let aead_key = vec_to_aead_key(&key)?;
    let nonce_arr = vec_to_nonce(&nonce)?;
    inner::decrypt_attachment(&ciphertext, &aead_key, &nonce_arr).map_err(Into::into)
}

/// Build the canonical MLS Application message JSON envelope:
/// `{ "v": 1, "text": "...", "attachments": [ ... ] }`. Caller
/// hands the result to `Group.createApplicationMessage(identity,
/// bytes)`.
///
/// Each `AttachmentRef`'s byte fields are converted to base64
/// inside this call — the JSON wire format uses base64 strings,
/// but the FFI surface stays raw-bytes-friendly for native
/// callers.
///
/// # Errors
///
/// - [`FfiError::Attachment`] on `serde_json` failure (impossible
///   for valid inputs) or on a wrong-length byte field in any
///   passed `AttachmentRef`.
#[uniffi::export]
pub fn compose_envelope(
    text: String,
    attachments: Vec<AttachmentRef>,
) -> Result<Vec<u8>, FfiError> {
    inner::compose(&text, refs_from_ffi(attachments)?).map_err(Into::into)
}

/// ADR-0052 — build a reply: an ordinary chat envelope that also names the
/// DS `messageId` it answers and carries a snapshot of the quoted message
/// (the author's display name and an excerpt of its text) as the replier
/// saw it. The snapshot is what every receiver renders — later edits or
/// deletion of the original never change the quote — and an excerpt is
/// always one level deep: replying to a reply quotes that reply's own
/// text, never the text it quoted. Over-long snapshots are truncated on a
/// character boundary; empty ones are not carried.
///
/// # Errors
///
/// - [`FfiError::Attachment`] on a wrong-length byte field in any passed
///   `AttachmentRef` (or on a `serde_json` failure, impossible for valid
///   inputs).
#[uniffi::export]
pub fn compose_reply_envelope(
    text: String,
    attachments: Vec<AttachmentRef>,
    reply_to: i64,
    reply_author: Option<String>,
    reply_excerpt: Option<String>,
) -> Result<Vec<u8>, FfiError> {
    let reply = inner::Reply {
        target: reply_to,
        author: reply_author,
        excerpt: reply_excerpt,
    };
    inner::compose_reply(&text, refs_from_ffi(attachments)?, &reply).map_err(Into::into)
}

/// ADR-0052 — build a forwarded message: the copied text and the
/// RE-ENCRYPTED attachments (the caller downloads, re-encrypts under fresh
/// keys and uploads before calling this - the refs passed here are the new
/// ones), with the original author's display name as a snapshot and, when
/// the forwarder holds the original, its location for a recipient who is also
/// in that conversation. A forward of a forward passes the ORIGINAL name and
/// origin, never the forwarder's.
///
/// # Errors
///
/// - [`FfiError::Attachment`] on a wrong-length byte field in any passed
///   `AttachmentRef` (or on a `serde_json` failure, impossible for valid
///   inputs).
#[uniffi::export]
pub fn compose_forward_envelope(
    text: String,
    attachments: Vec<AttachmentRef>,
    forwarded_from: Option<String>,
    origin_group_id_b64: Option<String>,
    origin_message_id: Option<i64>,
) -> Result<Vec<u8>, FfiError> {
    let origin = match (origin_group_id_b64, origin_message_id) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ForwardOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    let forward = inner::Forward {
        author: forwarded_from,
        origin,
    };
    inner::compose_forward(&text, refs_from_ffi(attachments)?, &forward).map_err(Into::into)
}

/// The optional parts a chat message may carry beside its text and
/// attachments, for [`compose_chat_envelope`]. Every field defaults to
/// `None`, so a caller names only the parts it has.
#[derive(uniffi::Record, Default)]
pub struct ChatEnvelopeParts {
    /// ADR-0052 — the DS `messageId` this message replies to. The two reply
    /// snapshots below are carried only when this is set.
    #[uniffi(default = None)]
    pub reply_to: Option<i64>,
    /// ADR-0052 — the quoted author's display name, a snapshot.
    #[uniffi(default = None)]
    pub reply_author: Option<String>,
    /// ADR-0052 — an excerpt of the quoted message's own text, a snapshot.
    #[uniffi(default = None)]
    pub reply_excerpt: Option<String>,
    /// ADR-0052 — on a forwarded message, the ORIGINAL author's display
    /// name as a snapshot.
    #[uniffi(default = None)]
    pub forwarded_from: Option<String>,
    /// ADR-0052 — base64 of the forward's origin group id; carried only
    /// together with `forward_origin_message_id`.
    #[uniffi(default = None)]
    pub forward_origin_group_b64: Option<String>,
    /// ADR-0052 — the DS `messageId` of the forwarded original.
    #[uniffi(default = None)]
    pub forward_origin_message_id: Option<i64>,
    /// ADR-0073 — base64 of the group id of the conversation this message
    /// was ORIGINALLY sent in, when it is a re-send into a successor
    /// one-to-one; carried only together with `resend_origin_message_id`.
    #[uniffi(default = None)]
    pub resend_origin_group_b64: Option<String>,
    /// ADR-0073 — the DS `messageId` of the original in that conversation.
    #[uniffi(default = None)]
    pub resend_origin_message_id: Option<i64>,
}

/// Build a chat message with any combination of its optional parts: a reply
/// (ADR-0052), a forward attribution (ADR-0052) and a re-send marker
/// (ADR-0073). [`compose_envelope`], [`compose_reply_envelope`] and
/// [`compose_forward_envelope`] are this call with one part set.
///
/// A message re-sent into a successor one-to-one passes the original's
/// attachments (re-encrypted, as for a forward), its reply snapshot and its
/// forward fields unchanged, plus the re-send origin: the group id and DS
/// `messageId` the original had. The marker is never rendered. A half pair
/// (origin group without message id or the reverse), or a group id that is
/// empty or over its bound, is not carried.
///
/// # Errors
///
/// - [`FfiError::Attachment`] on a wrong-length byte field in any passed
///   `AttachmentRef` (or on a `serde_json` failure, impossible for valid
///   inputs).
#[uniffi::export]
pub fn compose_chat_envelope(
    text: String,
    attachments: Vec<AttachmentRef>,
    parts: ChatEnvelopeParts,
) -> Result<Vec<u8>, FfiError> {
    let reply = parts.reply_to.map(|target| inner::Reply {
        target,
        author: parts.reply_author,
        excerpt: parts.reply_excerpt,
    });
    let origin = match (
        parts.forward_origin_group_b64,
        parts.forward_origin_message_id,
    ) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ForwardOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    let forward = (parts.forwarded_from.is_some() || origin.is_some()).then_some(inner::Forward {
        author: parts.forwarded_from,
        origin,
    });
    let resend = match (
        parts.resend_origin_group_b64,
        parts.resend_origin_message_id,
    ) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ResendOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    inner::compose_chat(
        &text,
        refs_from_ffi(attachments)?,
        reply.as_ref(),
        forward.as_ref(),
        resend.as_ref(),
    )
    .map_err(Into::into)
}

/// Convert FFI attachment refs to the wire shape: byte fields to base64,
/// lengths checked up front so a wrong-shape input fails fast at the FFI
/// rather than at JSON parse on the other side.
fn refs_from_ffi(attachments: Vec<AttachmentRef>) -> Result<Vec<inner::AttachmentRef>, FfiError> {
    attachments
        .into_iter()
        .map(|r| -> Result<inner::AttachmentRef, FfiError> {
            // Validate field lengths up front so a wrong-shape
            // input fails fast at the FFI rather than at JSON
            // parse on the other side.
            check_byte_fields("", &r.blob_id, &r.key, &r.nonce)?;
            let thumb = r
                .thumb
                .map(|t| -> Result<inner::ThumbRef, FfiError> {
                    check_byte_fields("thumb ", &t.blob_id, &t.key, &t.nonce)?;
                    Ok(inner::ThumbRef {
                        blob_id: B64.encode(&t.blob_id),
                        key: B64.encode(&t.key),
                        nonce: B64.encode(&t.nonce),
                        mime: t.mime,
                        size: t.size,
                        width: t.width,
                        height: t.height,
                    })
                })
                .transpose()?;
            // ADR-0047 bounds on the media fields are applied inside
            // `inner::compose`: an out-of-bound value is dropped, not an
            // error, exactly as every receiver would drop it.
            Ok(inner::AttachmentRef {
                blob_id: B64.encode(&r.blob_id),
                key: B64.encode(&r.key),
                nonce: B64.encode(&r.nonce),
                mime: r.mime,
                size: r.size,
                name: r.name,
                kind: r.kind,
                duration_ms: r.duration_ms,
                waveform: r.waveform.map(|w| B64.encode(w)),
                width: r.width,
                height: r.height,
                thumb,
            })
        })
        .collect()
}

/// ADR-0047 — the inline-playback allowlist: how an attachment with this
/// MIME type may be rendered inline, or `None` for a file chip. Decided
/// by MIME alone (parameters after `;` ignored, case-insensitive); never
/// pass `kind` here.
#[uniffi::export]
#[must_use]
pub fn inline_media_type(mime: String) -> Option<InlineMediaType> {
    inner::inline_media_type(&mime).map(Into::into)
}

/// Decode an MLS Application message JSON envelope. Returns the
/// extracted `(text, attachments)` packed into a [`ParsedEnvelope`].
///
/// Tolerates legacy pre-1.19 chat messages (bare UTF-8 with no
/// envelope) by returning `text = lossy_utf8(bytes), attachments
/// = []`. Surfaces an unknown wire version as the placeholder
/// `text = "[unsupported v$N message]"`.
///
/// Never fails — every malformed input collapses into one of the
/// fallback paths described above. Receivers get a sane string to
/// render in every case.
#[uniffi::export]
#[must_use]
pub fn parse_envelope(bytes: Vec<u8>) -> ParsedEnvelope {
    let (text, refs) = inner::parse(&bytes);
    // Malformed refs are skipped silently rather than failing the whole
    // envelope parse — the rest of the attachments may still be useful,
    // and the application path can decide whether a count-mismatch is a
    // user-visible issue.
    ParsedEnvelope {
        text,
        attachments: refs_to_ffi(refs),
    }
}

// ============================================================================
// ADR-0023 — edit / delete / reaction
// ============================================================================

/// The classification of a decoded message body (ADR-0023). Mirror
/// of [`lakko_attachments::MessageBody`] flattened for Swift/Kotlin:
/// the `kind` selects which of [`ParsedBody`]'s optional fields are
/// meaningful.
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
    /// ADR-0061 section 6 — a JSON chat envelope from a newer wire version.
    /// The version rides `target` (the read-receipt precedent for a small
    /// number on this record). `text` is empty: render the client's own
    /// catalog sentence, never core prose. Appended last so every earlier
    /// variant keeps its ordinal.
    Unsupported,
}

/// Output of [`parse_body_envelope`]: the classified message body
/// (ADR-0023). `text` + `attachments` are meaningful for `Chat` (and
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
    /// it is the envelope's wire version).
    pub target: Option<i64>,
    /// The reaction emoji (`Some` only for `Reaction`).
    pub emoji: Option<String>,
    /// ADR-0024 — the typing state (`Some` only for `Typing`).
    pub is_typing: Option<bool>,
    /// ADR-0052 — the DS `messageId` this message replies to (`Some` only
    /// for `Chat`, and only when the sender set it).
    pub reply_to: Option<i64>,
    /// ADR-0052 — the quoted author's display name as the replier wrote it:
    /// a snapshot and a courtesy label, never a verified identity.
    pub reply_author: Option<String>,
    /// ADR-0052 — an excerpt of the quoted message's own text, one level
    /// deep, unchanged by later edits or deletion of the original.
    pub reply_excerpt: Option<String>,
    /// ADR-0052 — on a forwarded message, the ORIGINAL author's display name
    /// as a snapshot: a label, never a verified identity or a route to them.
    pub forwarded_from: Option<String>,
    /// ADR-0052 — base64 of the origin conversation's group id, present only
    /// together with `forward_origin_message_id`; for a local jump by a
    /// recipient who is also in that conversation, never shown as such.
    pub forward_origin_group_b64: Option<String>,
    /// ADR-0052 — the DS `messageId` of the original in the origin
    /// conversation.
    pub forward_origin_message_id: Option<i64>,
    /// ADR-0073 — on a message re-sent into a successor one-to-one, base64
    /// of the ORIGINAL conversation's group id; present only together with
    /// `resend_origin_message_id`. Never shown: it lets a device that already
    /// holds the original show the message once.
    #[uniffi(default = None)]
    pub resend_origin_group_b64: Option<String>,
    /// ADR-0073 — the DS `messageId` of the original in that conversation.
    #[uniffi(default = None)]
    pub resend_origin_message_id: Option<i64>,
}

/// Build an EDIT envelope (ADR-0023) replacing the target message's
/// body with `new_text`. The author-only gate lives on the receiver.
///
/// # Errors
///
/// [`FfiError::Attachment`] on serialisation failure.
#[uniffi::export]
pub fn compose_edit_envelope(target: i64, new_text: String) -> Result<Vec<u8>, FfiError> {
    inner::compose_edit(target, &new_text).map_err(Into::into)
}

/// Build a DELETE (tombstone) envelope (ADR-0023) for `target`.
///
/// # Errors
///
/// [`FfiError::Attachment`] on serialisation failure.
#[uniffi::export]
pub fn compose_delete_envelope(target: i64) -> Result<Vec<u8>, FfiError> {
    inner::compose_delete(target).map_err(Into::into)
}

/// Build a REACTION envelope (ADR-0023): `emoji` to `target`.
///
/// # Errors
///
/// [`FfiError::Attachment`] if `emoji` is empty or exceeds
/// `MAX_EMOJI_BYTES`, or on serialisation failure.
#[uniffi::export]
pub fn compose_reaction_envelope(target: i64, emoji: String) -> Result<Vec<u8>, FfiError> {
    // Mechanical bridge after the core gained an explicit add / remove op
    // and an optional target-author key (UX reactions-and-emoji section 17):
    // this surface still sends an add without the key until the FFI work
    // exposes both parameters.
    inner::compose_reaction(target, &emoji, inner::ReactionAction::Add, None).map_err(Into::into)
}

/// Build an ephemeral TYPING envelope (ADR-0024).
///
/// # Errors
///
/// [`FfiError::Attachment`] on serialisation failure.
#[uniffi::export]
pub fn compose_typing_envelope(is_typing: bool) -> Result<Vec<u8>, FfiError> {
    inner::compose_typing(is_typing).map_err(Into::into)
}

/// Build a READ RECEIPT envelope (ADR-0024): read up to `up_to`.
///
/// # Errors
///
/// [`FfiError::Attachment`] on serialisation failure.
#[uniffi::export]
pub fn compose_read_receipt_envelope(up_to: i64) -> Result<Vec<u8>, FfiError> {
    inner::compose_read_receipt(up_to).map_err(Into::into)
}

/// Decode an MLS Application message body into a classified
/// [`ParsedBody`] (ADR-0023/0024). Never fails — a malformed /
/// ambiguous op degrades to `Chat`, the same fail-safe posture as
/// [`parse_envelope`].
#[uniffi::export]
#[must_use]
#[allow(clippy::too_many_lines)] // one struct literal per body kind
pub fn parse_body_envelope(bytes: Vec<u8>) -> ParsedBody {
    match inner::parse_body(&bytes) {
        inner::MessageBody::Chat {
            text,
            attachments,
            reply,
            forward,
            resend,
            ..
        } => ParsedBody {
            kind: MessageBodyKind::Chat,
            text,
            attachments: refs_to_ffi(attachments),
            target: None,
            emoji: None,
            is_typing: None,
            reply_to: reply.as_ref().map(|r| r.target),
            reply_author: reply.as_ref().and_then(|r| r.author.clone()),
            reply_excerpt: reply.and_then(|r| r.excerpt),
            forwarded_from: forward.as_ref().and_then(|f| f.author.clone()),
            forward_origin_group_b64: forward
                .as_ref()
                .and_then(|f| f.origin.as_ref().map(|o| o.group_id_b64.clone())),
            forward_origin_message_id: forward.and_then(|f| f.origin.map(|o| o.message_id)),
            resend_origin_group_b64: resend.as_ref().map(|r| r.group_id_b64.clone()),
            resend_origin_message_id: resend.map(|r| r.message_id),
        },
        inner::MessageBody::Edit { target, text } => ParsedBody {
            kind: MessageBodyKind::Edit,
            text,
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::Delete { target } => ParsedBody {
            kind: MessageBodyKind::Delete,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::Reaction { target, emoji, .. } => ParsedBody {
            kind: MessageBodyKind::Reaction,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: Some(emoji),
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::Typing { is_typing } => ParsedBody {
            kind: MessageBodyKind::Typing,
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: Some(is_typing),
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::ReadReceipt { up_to } => ParsedBody {
            kind: MessageBodyKind::ReadReceipt,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(up_to),
            emoji: None,
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::Unsupported { version } => ParsedBody {
            kind: MessageBodyKind::Unsupported,
            text: String::new(),
            attachments: Vec::new(),
            target: Some(i64::from(version)),
            emoji: None,
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
        inner::MessageBody::ServiceEnvelope => ParsedBody {
            kind: MessageBodyKind::ServiceEnvelope,
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: None,
            reply_to: None,
            reply_author: None,
            reply_excerpt: None,
            forwarded_from: None,
            forward_origin_group_b64: None,
            forward_origin_message_id: None,
            resend_origin_group_b64: None,
            resend_origin_message_id: None,
        },
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Decode inner `AttachmentRef`s into the FFI flat shape, dropping
/// any malformed ref silently (the same policy as [`parse_envelope`]).
fn refs_to_ffi(refs: Vec<inner::AttachmentRef>) -> Vec<AttachmentRef> {
    refs.into_iter()
        .filter_map(|r| {
            let blob_id = r.decode_blob_id().ok()?;
            let key = r.decode_key().ok()?;
            let nonce = r.decode_nonce().ok()?;
            // ADR-0047 media fields arrive already bounded by the core
            // parser; a thumb that still fails to decode is dropped alone.
            let waveform = r.decode_waveform();
            let thumb = r.thumb.as_ref().and_then(thumb_to_ffi);
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

fn thumb_to_ffi(t: &inner::ThumbRef) -> Option<ThumbRef> {
    Some(ThumbRef {
        blob_id: t.decode_blob_id().ok()?.to_vec(),
        key: t.decode_key().ok()?.as_bytes().to_vec(),
        nonce: t.decode_nonce().ok()?.to_vec(),
        mime: t.mime.clone(),
        size: t.size,
        width: t.width,
        height: t.height,
    })
}

/// Wrong-length raw byte fields fail the compose call (a caller bug),
/// with `prefix` naming a nested ref (`"thumb "`).
fn check_byte_fields(
    prefix: &str,
    blob_id: &[u8],
    key: &[u8],
    nonce: &[u8],
) -> Result<(), FfiError> {
    for (field, got, want) in [
        ("blob_id", blob_id.len(), inner::BLOB_ID_LEN),
        ("key", key.len(), inner::ATTACHMENT_KEY_LEN),
        ("nonce", nonce.len(), inner::ATTACHMENT_NONCE_LEN),
    ] {
        if got != want {
            return Err(FfiError::Attachment(format!(
                "{prefix}{field} length {got} != {want}"
            )));
        }
    }
    Ok(())
}

fn vec_to_aead_key(bytes: &[u8]) -> Result<AeadKey, FfiError> {
    if bytes.len() != inner::ATTACHMENT_KEY_LEN {
        return Err(FfiError::Attachment(format!(
            "key length {} != {}",
            bytes.len(),
            inner::ATTACHMENT_KEY_LEN
        )));
    }
    let mut arr = [0u8; inner::ATTACHMENT_KEY_LEN];
    arr.copy_from_slice(bytes);
    Ok(AeadKey::new(arr))
}

fn vec_to_nonce(bytes: &[u8]) -> Result<[u8; inner::ATTACHMENT_NONCE_LEN], FfiError> {
    if bytes.len() != inner::ATTACHMENT_NONCE_LEN {
        return Err(FfiError::Attachment(format!(
            "nonce length {} != {}",
            bytes.len(),
            inner::ATTACHMENT_NONCE_LEN
        )));
    }
    let mut arr = [0u8; inner::ATTACHMENT_NONCE_LEN];
    arr.copy_from_slice(bytes);
    Ok(arr)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip_through_ffi_shape() {
        let plaintext = b"hello attachments";
        let enc = encrypt_attachment(plaintext.to_vec()).unwrap();
        // ChaCha20-Poly1305 appends a 16-byte authentication tag.
        assert_eq!(enc.ciphertext.len(), plaintext.len() + 16);
        assert_eq!(enc.key.len(), inner::ATTACHMENT_KEY_LEN);
        assert_eq!(enc.nonce.len(), inner::ATTACHMENT_NONCE_LEN);
        assert_eq!(enc.blob_id.len(), inner::BLOB_ID_LEN);
        // Round-trip through the FFI shape (byte arrays in + out).
        let recovered =
            decrypt_attachment(enc.ciphertext.clone(), enc.key.clone(), enc.nonce.clone()).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn decrypt_rejects_wrong_length_key() {
        let err = decrypt_attachment(vec![0u8; 16], vec![0u8; 8], vec![0u8; 12]).unwrap_err();
        match err {
            FfiError::Attachment(msg) => assert!(msg.contains("key length")),
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn decrypt_rejects_wrong_length_nonce() {
        let err = decrypt_attachment(vec![0u8; 16], vec![0u8; 32], vec![0u8; 8]).unwrap_err();
        match err {
            FfiError::Attachment(msg) => assert!(msg.contains("nonce length")),
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn compose_parse_round_trip_text_only() {
        let bytes = compose_envelope("hello".to_string(), Vec::new()).unwrap();
        let parsed = parse_envelope(bytes);
        assert_eq!(parsed.text, "hello");
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn compose_parse_round_trip_with_attachment() {
        let enc = encrypt_attachment(b"image bytes".to_vec()).unwrap();
        let attachment = AttachmentRef {
            blob_id: enc.blob_id.clone(),
            key: enc.key.clone(),
            nonce: enc.nonce.clone(),
            mime: Some("image/png".to_string()),
            size: 11,
            name: Some("cat.png".to_string()),
            ..bare_media()
        };
        let bytes = compose_envelope("look".to_string(), vec![attachment.clone()]).unwrap();
        let parsed = parse_envelope(bytes);
        assert_eq!(parsed.text, "look");
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].blob_id, attachment.blob_id);
        assert_eq!(parsed.attachments[0].key, attachment.key);
        assert_eq!(parsed.attachments[0].nonce, attachment.nonce);
        assert_eq!(parsed.attachments[0].mime, attachment.mime);
        assert_eq!(parsed.attachments[0].size, attachment.size);
        assert_eq!(parsed.attachments[0].name, attachment.name);
    }

    #[test]
    fn compose_rejects_wrong_length_byte_field() {
        let bad = AttachmentRef {
            blob_id: vec![0u8; 16], // wrong length — 32 expected
            ..bare_media()
        };
        let err = compose_envelope("x".to_string(), vec![bad]).unwrap_err();
        match err {
            FfiError::Attachment(msg) => assert!(msg.contains("blob_id length")),
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn parse_legacy_bare_utf8_yields_text_only() {
        let parsed = parse_envelope(b"plain old text".to_vec());
        assert_eq!(parsed.text, "plain old text");
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn parse_unknown_version_surfaces_placeholder() {
        let evil = br#"{"v":99,"text":"future","attachments":[]}"#.to_vec();
        let parsed = parse_envelope(evil);
        assert_eq!(parsed.text, "[unsupported v99 message]");
        assert!(parsed.attachments.is_empty());
    }

    // ---- ADR-0023 ----

    #[test]
    fn edit_envelope_round_trips_through_parse_body() {
        let bytes = compose_edit_envelope(42, "fixed".to_string()).unwrap();
        let body = parse_body_envelope(bytes);
        assert_eq!(body.kind, MessageBodyKind::Edit);
        assert_eq!(body.target, Some(42));
        assert_eq!(body.text, "fixed");
    }

    #[test]
    fn delete_envelope_round_trips_through_parse_body() {
        let bytes = compose_delete_envelope(7).unwrap();
        let body = parse_body_envelope(bytes);
        assert_eq!(body.kind, MessageBodyKind::Delete);
        assert_eq!(body.target, Some(7));
    }

    #[test]
    fn reaction_envelope_round_trips_through_parse_body() {
        let bytes = compose_reaction_envelope(9, "\u{1F44D}".to_string()).unwrap();
        let body = parse_body_envelope(bytes);
        assert_eq!(body.kind, MessageBodyKind::Reaction);
        assert_eq!(body.target, Some(9));
        assert_eq!(body.emoji, Some("\u{1F44D}".to_string()));
    }

    #[test]
    fn plain_envelope_parses_as_chat_body() {
        let bytes = compose_envelope("hi".to_string(), Vec::new()).unwrap();
        let body = parse_body_envelope(bytes);
        assert_eq!(body.kind, MessageBodyKind::Chat);
        assert_eq!(body.text, "hi");
        assert_eq!(body.target, None);
    }

    #[test]
    fn newer_wire_version_is_unsupported_with_no_text() {
        let body = parse_body_envelope(br#"{"v":7,"text":"future"}"#.to_vec());
        assert_eq!(body.kind, MessageBodyKind::Unsupported);
        assert_eq!(body.target, Some(7));
        assert!(body.text.is_empty());
        assert!(body.attachments.is_empty());
    }

    #[test]
    fn typing_envelope_round_trips_through_parse_body() {
        let body = parse_body_envelope(compose_typing_envelope(true).unwrap());
        assert_eq!(body.kind, MessageBodyKind::Typing);
        assert_eq!(body.is_typing, Some(true));
        let off = parse_body_envelope(compose_typing_envelope(false).unwrap());
        assert_eq!(off.kind, MessageBodyKind::Typing);
        assert_eq!(off.is_typing, Some(false));
    }

    #[test]
    fn read_receipt_envelope_round_trips_through_parse_body() {
        let body = parse_body_envelope(compose_read_receipt_envelope(1042).unwrap());
        assert_eq!(body.kind, MessageBodyKind::ReadReceipt);
        assert_eq!(body.target, Some(1042));
    }

    // ---- ADR-0047 ----

    /// A well-formed ref with no media metadata, for struct-update syntax.
    fn bare_media() -> AttachmentRef {
        AttachmentRef {
            blob_id: vec![0u8; 32],
            key: vec![0u8; 32],
            nonce: vec![0u8; 12],
            mime: None,
            size: 0,
            name: None,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
        }
    }

    fn thumb_from(bytes: &[u8]) -> ThumbRef {
        let enc = encrypt_attachment(bytes.to_vec()).unwrap();
        ThumbRef {
            blob_id: enc.blob_id,
            key: enc.key,
            nonce: enc.nonce,
            mime: Some("image/jpeg".to_string()),
            size: bytes.len() as u64,
            width: Some(240),
            height: Some(240),
        }
    }

    #[test]
    fn media_fields_round_trip_through_both_parsers() {
        let enc = encrypt_attachment(b"note bytes".to_vec()).unwrap();
        let thumb = thumb_from(b"poster");
        let r = AttachmentRef {
            blob_id: enc.blob_id,
            key: enc.key,
            nonce: enc.nonce,
            mime: Some("video/mp4".to_string()),
            size: 10,
            name: Some("note-1.mp4".to_string()),
            kind: Some("video_note".to_string()),
            duration_ms: Some(9000),
            waveform: Some(vec![1, 2, 3]),
            width: Some(480),
            height: Some(480),
            thumb: Some(thumb.clone()),
        };
        let bytes = compose_envelope("c".to_string(), vec![r.clone()]).unwrap();
        for got in [
            parse_envelope(bytes.clone()).attachments,
            parse_body_envelope(bytes).attachments,
        ] {
            let got = &got[0];
            assert_eq!(got.kind, r.kind);
            assert_eq!(got.duration_ms, Some(9000));
            assert_eq!(got.waveform, Some(vec![1, 2, 3]));
            assert_eq!((got.width, got.height), (Some(480), Some(480)));
            let t = got.thumb.as_ref().unwrap();
            assert_eq!(
                (&t.blob_id, &t.key, &t.nonce),
                (&thumb.blob_id, &thumb.key, &thumb.nonce)
            );
            assert_eq!(
                (t.size, t.width, t.height, &t.mime),
                (6, Some(240), Some(240), &thumb.mime)
            );
        }
    }

    #[test]
    fn out_of_bound_media_fields_are_dropped_not_errors() {
        let r = AttachmentRef {
            kind: Some("voice".to_string()),
            duration_ms: Some(15 * 60 * 1000 + 1),
            waveform: Some(vec![0u8; 129]),
            width: Some(4097),
            height: Some(10),
            ..bare_media()
        };
        let parsed = parse_envelope(compose_envelope("c".to_string(), vec![r]).unwrap());
        let got = &parsed.attachments[0];
        assert_eq!(got.kind.as_deref(), Some("voice"));
        assert_eq!((got.duration_ms, got.waveform.as_ref()), (None, None));
        assert_eq!((got.width, got.height), (None, Some(10)));
    }

    #[test]
    fn compose_rejects_wrong_length_thumb_byte_field() {
        let mut thumb = thumb_from(b"poster");
        thumb.nonce = vec![0u8; 5];
        let r = AttachmentRef {
            thumb: Some(thumb),
            ..bare_media()
        };
        match compose_envelope("x".to_string(), vec![r]).unwrap_err() {
            FfiError::Attachment(msg) => assert!(msg.contains("thumb nonce length"), "{msg}"),
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn inline_media_type_follows_the_core_allowlist() {
        assert_eq!(
            inline_media_type("audio/mp4".to_string()),
            Some(InlineMediaType::Audio)
        );
        assert_eq!(
            inline_media_type("video/mp4;codecs=avc1".to_string()),
            Some(InlineMediaType::Video)
        );
        assert_eq!(
            inline_media_type("image/gif".to_string()),
            Some(InlineMediaType::Image)
        );
        assert_eq!(inline_media_type("image/svg+xml".to_string()), None);
        assert_eq!(inline_media_type("text/html".to_string()), None);
    }

    #[test]
    fn reply_round_trips_and_stays_a_chat() {
        let wire = compose_reply_envelope(
            "sure".to_string(),
            vec![],
            41,
            Some("Alice".to_string()),
            Some("lunch?".to_string()),
        )
        .unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.kind, MessageBodyKind::Chat);
        assert_eq!(body.text, "sure");
        assert_eq!(body.reply_to, Some(41));
        assert_eq!(body.reply_author.as_deref(), Some("Alice"));
        assert_eq!(body.reply_excerpt.as_deref(), Some("lunch?"));
        // The reply is not an op: `target` is what edit / delete / react use.
        assert_eq!(body.target, None);
    }

    #[test]
    fn a_plain_chat_carries_no_reply() {
        let wire = compose_envelope("hi".to_string(), vec![]).unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.reply_to, None);
        assert_eq!(body.reply_author, None);
        assert_eq!(body.reply_excerpt, None);
    }

    #[test]
    fn resend_marker_round_trips_beside_reply_and_forward() {
        let wire = compose_chat_envelope(
            "again".to_string(),
            vec![],
            ChatEnvelopeParts {
                reply_to: Some(5),
                reply_author: Some("Bob".to_string()),
                forwarded_from: Some("Alice".to_string()),
                resend_origin_group_b64: Some("b2xk".to_string()),
                resend_origin_message_id: Some(17),
                ..ChatEnvelopeParts::default()
            },
        )
        .unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.kind, MessageBodyKind::Chat);
        assert_eq!(body.text, "again");
        assert_eq!(body.reply_to, Some(5));
        assert_eq!(body.reply_author.as_deref(), Some("Bob"));
        assert_eq!(body.forwarded_from.as_deref(), Some("Alice"));
        assert_eq!(body.resend_origin_group_b64.as_deref(), Some("b2xk"));
        assert_eq!(body.resend_origin_message_id, Some(17));
        // Half a marker is no marker.
        let wire = compose_chat_envelope(
            "x".to_string(),
            vec![],
            ChatEnvelopeParts {
                resend_origin_group_b64: Some("b2xk".to_string()),
                ..ChatEnvelopeParts::default()
            },
        )
        .unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.resend_origin_group_b64, None);
        assert_eq!(body.resend_origin_message_id, None);
        // No parts is the plain envelope, byte for byte.
        assert_eq!(
            compose_chat_envelope("hi".to_string(), vec![], ChatEnvelopeParts::default()).unwrap(),
            compose_envelope("hi".to_string(), vec![]).unwrap()
        );
        // An op that also carries the marker is still that op.
        let body = parse_body_envelope(
            br#"{"v":1,"delete_of":7,"resend_origin_group":"b2xk","resend_origin_message":3}"#
                .to_vec(),
        );
        assert_eq!(body.kind, MessageBodyKind::Delete);
        assert_eq!(body.resend_origin_message_id, None);
    }

    #[test]
    fn forward_round_trips_and_stays_a_chat() {
        let wire = compose_forward_envelope(
            "copied".to_string(),
            vec![],
            Some("Alice".to_string()),
            Some("Z3JvdXA=".to_string()),
            Some(41),
        )
        .unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.kind, MessageBodyKind::Chat);
        assert_eq!(body.forwarded_from.as_deref(), Some("Alice"));
        assert_eq!(body.forward_origin_group_b64.as_deref(), Some("Z3JvdXA="));
        assert_eq!(body.forward_origin_message_id, Some(41));
        assert_eq!(body.target, None);
        assert_eq!(body.reply_to, None);
        // Half an origin is no origin; the name still travels.
        let wire = compose_forward_envelope(
            "x".to_string(),
            vec![],
            Some("Alice".to_string()),
            Some("Z3JvdXA=".to_string()),
            None,
        )
        .unwrap();
        let body = parse_body_envelope(wire);
        assert_eq!(body.forwarded_from.as_deref(), Some("Alice"));
        assert_eq!(body.forward_origin_group_b64, None);
        assert_eq!(body.forward_origin_message_id, None);
    }
}
