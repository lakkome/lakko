// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-attachments` (Phase 1.19b).
//!
//! Mirror of the UniFFI module at
//! `crates/lakko-ffi-uniffi/src/attachments.rs` — same operations,
//! same wire shape, JS-idiomatic naming via `js_name` attrs.
//!
//! ## Why ChaCha20-Poly1305 via wasm and not WebCrypto
//!
//! `crypto.subtle` shipped AES-GCM in 2014 but never picked up
//! ChaCha20-Poly1305 — every browser still ships only AES.
//! Going through `lakko-attachments` (Rust) keeps the algorithm
//! identical across all three platforms (iOS uses CryptoKit's
//! native ChaCha20; Android uses Bouncy Castle / Tink; Web uses
//! `lakko-attachments` via this wasm wrapper). The 12-byte
//! nonce, 32-byte CEK, and 16-byte tag layout is byte-identical
//! everywhere — same encryption can be done on iOS and the
//! ciphertext decrypts on Web.
//!
//! ## API shape (JS)
//!
//! ```ts
//! import { encryptAttachment, decryptAttachment,
//!          composeEnvelope, parseEnvelope } from "@lakko/wasm";
//!
//! const enc = encryptAttachment(plaintextBytes);
//! // enc.ciphertext / enc.key / enc.nonce / enc.blobId — Uint8Array
//!
//! const ref = {
//!   blobId:  enc.blobId,
//!   key:     enc.key,
//!   nonce:   enc.nonce,
//!   mime:    "image/png",
//!   size:    BigInt(plaintextBytes.length),
//!   name:    "cat.png",
//! };
//! const wire = composeEnvelope("look at this", [ref]);
//! // hand `wire` to Group.createApplicationMessage(identity, wire)
//!
//! // ... on receive ...
//! const parsed = parseEnvelope(applicationPayload);
//! // parsed.text — string; parsed.attachments — Array<AttachmentRef>
//! for (const a of parsed.attachments) {
//!   const ct = await fetch(`/blob/${hex(a.blobId)}`).then(r =>
//!     r.arrayBuffer()).then(b => new Uint8Array(b));
//!   const pt = decryptAttachment(ct, a.key, a.nonce);
//!   // render(pt, a.mime, a.name)
//! }
//! ```

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attachments as inner;
use lakko_crypto::aead::AeadKey;
use wasm_bindgen::prelude::*;

fn attachment_err(e: inner::AttachmentError) -> JsError {
    JsError::new(&format!("{e}"))
}

fn boundary_err(msg: String) -> JsError {
    JsError::new(&msg)
}

// ============================================================================
// EncryptedAttachment — handle with byte-array getters
// ============================================================================

/// Output of [`encrypt_attachment`]. Carries ciphertext + CEK +
/// nonce + content-addressed `blob_id`.
///
/// JS reads the four byte fields via the `ciphertext` / `key` /
/// `nonce` / `blobId` getters; the wasm-bindgen generated `.d.ts`
/// types them as `Uint8Array`.
#[wasm_bindgen]
pub struct EncryptedAttachment {
    ciphertext: Vec<u8>,
    key: Vec<u8>,
    nonce: Vec<u8>,
    blob_id: Vec<u8>,
}

#[wasm_bindgen]
impl EncryptedAttachment {
    /// Ciphertext bytes — `ChaCha20-Poly1305(plaintext)` with AAD
    /// empty. PUT verbatim to `/blob/<hex(blobId)>`.
    #[wasm_bindgen(getter)]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }

    /// Fresh random 32-byte ChaCha20-Poly1305 key (CEK).
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.key.clone()
    }

    /// Fresh random 12-byte AEAD nonce.
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce.clone()
    }

    /// `SHA-256(ciphertext)` — the storage service's content
    /// address. 32 bytes raw; caller hex-encodes for the URL.
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.blob_id.clone()
    }
}

// ============================================================================
// AttachmentRef — handle with byte-array + scalar getters
// ============================================================================

/// One attachment reference inside an MLS Application message
/// envelope. Constructed via [`AttachmentRef::new`] on the send
/// path; obtained via [`ParsedEnvelope::attachments`] on the
/// receive path.
///
/// ADR-0047 media metadata (`kind`, `durationMs`, `waveform`, `width`,
/// `height`, `thumb`) is optional and set after construction, so the
/// constructor's signature is unchanged:
///
/// ```ts
/// const r = new AttachmentRef(blobId, key, nonce, "audio/mp4", size, "voice-1.m4a");
/// r.kind = "voice";
/// r.durationMs = 4200;
/// r.waveform = amplitudes; // Uint8Array, 1..=128 bytes
/// ```
///
/// Out-of-bound values are not errors: `EnvelopeBuilder.build` drops them,
/// as every receiver would.
#[wasm_bindgen]
#[derive(Clone)]
pub struct AttachmentRef {
    blob_id: Vec<u8>,
    key: Vec<u8>,
    nonce: Vec<u8>,
    mime: Option<String>,
    size: u64,
    name: Option<String>,
    kind: Option<String>,
    duration_ms: Option<u32>,
    waveform: Option<Vec<u8>>,
    width: Option<u32>,
    height: Option<u32>,
    thumb: Option<ThumbRef>,
    title: Option<String>,
    artist: Option<String>,
}

/// ADR-0047 — a poster-frame blob attached to an [`AttachmentRef`]: its
/// own encrypted blob under its own CEK. Attach with
/// `AttachmentRef.setThumb`; read with the `thumb` getter.
#[wasm_bindgen]
#[derive(Clone)]
pub struct ThumbRef {
    blob_id: Vec<u8>,
    key: Vec<u8>,
    nonce: Vec<u8>,
    mime: Option<String>,
    size: u64,
    width: Option<u32>,
    height: Option<u32>,
}

#[wasm_bindgen]
impl ThumbRef {
    /// Construct from raw byte fields + metadata. Field lengths are
    /// validated when the owning ref is added to an `EnvelopeBuilder`;
    /// `size` (1..=256 KiB) and the dimensions (1..=4096) are bounded at
    /// build time, where an out-of-bound thumb is dropped.
    #[wasm_bindgen(constructor)]
    pub fn new(
        blob_id: Vec<u8>,
        key: Vec<u8>,
        nonce: Vec<u8>,
        mime: Option<String>,
        size: u64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            blob_id,
            key,
            nonce,
            mime,
            size,
            width,
            height,
        }
    }

    /// Raw 32-byte SHA-256 blob id of the poster blob.
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.blob_id.clone()
    }

    /// Raw 32-byte CEK of the poster blob.
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.key.clone()
    }

    /// Raw 12-byte AEAD nonce of the poster blob.
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce.clone()
    }

    /// Poster MIME type, `undefined` when not set. Check it with
    /// `inlineMediaType` before rendering.
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        self.mime.clone()
    }

    /// Poster plaintext size in bytes (`BigInt`).
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Poster pixel width, `undefined` when absent.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.width
    }

    /// Poster pixel height, `undefined` when absent.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.height
    }
}

/// ADR-0047 — how an attachment may be rendered inline. Returned by
/// [`inline_media_type`]; `undefined` there means a file chip.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineMediaType {
    /// Render as an image.
    Image = 0,
    /// Play as audio.
    Audio = 1,
    /// Play as video.
    Video = 2,
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

/// ADR-0047 — the inline-playback allowlist: how an attachment with this
/// MIME type may be rendered inline, or `undefined` for a file chip.
/// Decided by MIME alone (parameters after `;` ignored, case-insensitive);
/// never pass `kind` here.
#[wasm_bindgen(js_name = inlineMediaType)]
#[must_use]
pub fn inline_media_type(mime: &str) -> Option<InlineMediaType> {
    inner::inline_media_type(mime).map(Into::into)
}

#[wasm_bindgen]
impl AttachmentRef {
    /// Construct from raw byte fields + metadata. Field-length
    /// validation happens at `composeEnvelope` time, not here —
    /// keeping the constructor permissive lets callers build refs
    /// incrementally (e.g. compute the SHA-256 separately from
    /// the CEK derivation).
    #[wasm_bindgen(constructor)]
    pub fn new(
        blob_id: Vec<u8>,
        key: Vec<u8>,
        nonce: Vec<u8>,
        mime: Option<String>,
        size: u64,
        name: Option<String>,
    ) -> Self {
        Self {
            blob_id,
            key,
            nonce,
            mime,
            size,
            name,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
            title: None,
            artist: None,
        }
    }

    /// ADR-0090 — an `audio` attachment's title tag (<= 255 bytes); kept on
    /// the wire only on kind `"audio"`.
    #[wasm_bindgen(getter)]
    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }

    /// Set the audio title tag.
    #[wasm_bindgen(setter)]
    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    /// ADR-0090 — an `audio` attachment's artist tag (<= 255 bytes).
    #[wasm_bindgen(getter)]
    pub fn artist(&self) -> Option<String> {
        self.artist.clone()
    }

    /// Set the audio artist tag.
    #[wasm_bindgen(setter)]
    pub fn set_artist(&mut self, artist: Option<String>) {
        self.artist = artist;
    }

    /// ADR-0047 — `"voice"`, `"video_note"`, `"gif"`, another (unknown)
    /// value that renders as a file, or `undefined` for a plain file.
    /// Chooses the bubble only; inline playback is `inlineMediaType(mime)`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> Option<String> {
        self.kind.clone()
    }

    /// Set (or clear with `undefined`) the media kind.
    #[wasm_bindgen(setter)]
    pub fn set_kind(&mut self, kind: Option<String>) {
        self.kind = kind;
    }

    /// ADR-0047 — playback length in ms. Survives only on a `voice`
    /// (<= 15 min) or `video_note` (<= 60 s) ref.
    #[wasm_bindgen(getter, js_name = durationMs)]
    pub fn duration_ms(&self) -> Option<u32> {
        self.duration_ms
    }

    /// Set (or clear with `undefined`) `durationMs`.
    #[wasm_bindgen(setter, js_name = durationMs)]
    pub fn set_duration_ms(&mut self, duration_ms: Option<u32>) {
        self.duration_ms = duration_ms;
    }

    /// ADR-0047 — raw waveform amplitude bytes (1..=128), `undefined` when
    /// absent.
    #[wasm_bindgen(getter)]
    pub fn waveform(&self) -> Option<Vec<u8>> {
        self.waveform.clone()
    }

    /// Set (or clear with `undefined`) the waveform bytes.
    #[wasm_bindgen(setter)]
    pub fn set_waveform(&mut self, waveform: Option<Vec<u8>>) {
        self.waveform = waveform;
    }

    /// ADR-0047 — pixel width (1..=4096), `undefined` when absent.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.width
    }

    /// Set (or clear with `undefined`) the pixel width.
    #[wasm_bindgen(setter)]
    pub fn set_width(&mut self, width: Option<u32>) {
        self.width = width;
    }

    /// ADR-0047 — pixel height (1..=4096), `undefined` when absent.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.height
    }

    /// Set (or clear with `undefined`) the pixel height.
    #[wasm_bindgen(setter)]
    pub fn set_height(&mut self, height: Option<u32>) {
        self.height = height;
    }

    /// ADR-0047 — the poster-frame thumb (a copy), `undefined` when absent.
    #[wasm_bindgen(getter)]
    pub fn thumb(&self) -> Option<ThumbRef> {
        self.thumb.clone()
    }

    /// Attach a copy of `thumb`. A method rather than a setter so the
    /// caller's `ThumbRef` handle stays valid (a by-value setter would
    /// consume it).
    #[wasm_bindgen(js_name = setThumb)]
    pub fn set_thumb(&mut self, thumb: &ThumbRef) {
        self.thumb = Some(thumb.clone());
    }

    /// Remove the thumb.
    #[wasm_bindgen(js_name = clearThumb)]
    pub fn clear_thumb(&mut self) {
        self.thumb = None;
    }

    /// Raw 32-byte SHA-256 blob id.
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.blob_id.clone()
    }

    /// Raw 32-byte CEK.
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.key.clone()
    }

    /// Raw 12-byte AEAD nonce.
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce.clone()
    }

    /// Optional MIME type (e.g. `"image/jpeg"`). `undefined` when
    /// not set.
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        self.mime.clone()
    }

    /// Plaintext size in bytes. JS represents `u64` as `BigInt` on
    /// the wasm boundary.
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Optional human-visible filename. `undefined` when not set.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }
}

// ============================================================================
// ParsedEnvelope — output of parseEnvelope
// ============================================================================

/// Output of [`parse_envelope`]. Carries the user-visible text +
/// zero or more [`AttachmentRef`]s.
#[wasm_bindgen]
pub struct ParsedEnvelope {
    text: String,
    attachments: Vec<AttachmentRef>,
}

#[wasm_bindgen]
impl ParsedEnvelope {
    /// The user-visible text. Empty string is valid (attachment-
    /// only message).
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text.clone()
    }

    /// Number of attachments — exposed as a getter so JS can size
    /// arrays / progress indicators before iterating.
    #[wasm_bindgen(getter, js_name = attachmentCount)]
    pub fn attachment_count(&self) -> u32 {
        u32::try_from(self.attachments.len()).unwrap_or(u32::MAX)
    }

    /// Pull one attachment by index. wasm-bindgen does not export
    /// `Vec<T>` of non-`Copy` `T` over the boundary directly, so
    /// we expose an index-based accessor — JS iterates `for (let
    /// i = 0; i < env.attachmentCount; i++) env.attachment(i)`.
    ///
    /// # Errors
    ///
    /// `RangeError` on out-of-bounds `i`.
    #[wasm_bindgen]
    pub fn attachment(&self, i: u32) -> Result<AttachmentRef, JsError> {
        let idx = usize::try_from(i).map_err(|_| boundary_err(format!("bad index {i}")))?;
        self.attachments
            .get(idx)
            .cloned()
            .ok_or_else(|| boundary_err(format!("attachment index {i} out of bounds")))
    }
}

// ============================================================================
// Free functions (encrypt / decrypt / compose / parse)
// ============================================================================

/// Encrypt `plaintext` under a fresh random CEK + nonce. Mirror of
/// the Rust crate's `encrypt_attachment`.
#[wasm_bindgen(js_name = encryptAttachment)]
pub fn encrypt_attachment(plaintext: &[u8]) -> Result<EncryptedAttachment, JsError> {
    let enc = inner::encrypt_attachment(plaintext).map_err(attachment_err)?;
    Ok(EncryptedAttachment {
        ciphertext: enc.ciphertext,
        key: enc.key.as_bytes().to_vec(),
        nonce: enc.nonce.to_vec(),
        blob_id: enc.blob_id.to_vec(),
    })
}

/// Decrypt `ciphertext` under `key` + `nonce`. Mirror of the Rust
/// crate's `decrypt_attachment`.
#[wasm_bindgen(js_name = decryptAttachment)]
pub fn decrypt_attachment(ciphertext: &[u8], key: &[u8], nonce: &[u8]) -> Result<Vec<u8>, JsError> {
    let aead_key = bytes_to_aead_key(key)?;
    let nonce_arr = bytes_to_nonce(nonce)?;
    inner::decrypt_attachment(ciphertext, &aead_key, &nonce_arr).map_err(attachment_err)
}

/// Build the canonical MLS Application message JSON envelope.
/// Mirror of `lakko_attachments::compose`.
///
/// `attachments` is an array of [`AttachmentRef`] instances. The
/// wasm-bindgen `Vec<JsValue>`-marshalling pattern would force us
/// to manually downcast each entry; the simpler shape is to take
/// the refs one at a time via the [`EnvelopeBuilder`] helper
/// below. For the common case of a single-attachment message,
/// callers can use [`compose_envelope_one`] directly.
///
/// JS idiom for the general case:
///
/// ```ts
/// const b = new EnvelopeBuilder("text body");
/// for (const r of attachmentRefs) b.add(r);
/// const wire = b.build();
/// ```
///
/// The optional parts of a chat message are set on the builder before
/// building and may be combined freely: `setReply`, `setForward` (both
/// ADR-0052) and `setResendOrigin` (ADR-0073). `build()` emits whatever was
/// set; `buildReply` / `buildForward` set their own part and keep the rest.
///
/// ```ts
/// const b = new EnvelopeBuilder(original.text);
/// b.setReply(original.replyTo, original.replyAuthor, original.replyExcerpt);
/// b.setResendOrigin(oldGroupIdB64, original.messageId);
/// const wire = b.build();
/// ```
#[wasm_bindgen]
pub struct EnvelopeBuilder {
    text: String,
    refs: Vec<inner::AttachmentRef>,
    reply: Option<inner::Reply>,
    forward: Option<inner::Forward>,
    resend: Option<inner::ResendOrigin>,
}

#[wasm_bindgen]
impl EnvelopeBuilder {
    /// Start a new envelope with the given user-visible text.
    /// Empty string is valid (attachment-only message).
    #[wasm_bindgen(constructor)]
    pub fn new(text: String) -> Self {
        Self {
            text,
            refs: Vec::new(),
            reply: None,
            forward: None,
            resend: None,
        }
    }

    /// ADR-0052 — make the message a reply to the DS `messageId` `replyTo`
    /// (a `bigint`), carrying the two snapshots; see `buildReply`. Replaces
    /// a reply set earlier.
    #[wasm_bindgen(js_name = setReply)]
    pub fn set_reply(
        &mut self,
        reply_to: i64,
        reply_author: Option<String>,
        reply_excerpt: Option<String>,
    ) {
        self.reply = Some(inner::Reply {
            target: reply_to,
            author: reply_author,
            excerpt: reply_excerpt,
        });
    }

    /// ADR-0052 — give the message a forward attribution; see
    /// `buildForward`. The origin is carried only when both halves are
    /// given; with neither a name nor a whole origin, any forward set
    /// earlier is cleared.
    #[wasm_bindgen(js_name = setForward)]
    pub fn set_forward(
        &mut self,
        forwarded_from: Option<String>,
        origin_group_id_b64: Option<String>,
        origin_message_id: Option<i64>,
    ) {
        self.forward = forward_from_parts(forwarded_from, origin_group_id_b64, origin_message_id);
    }

    /// ADR-0073 — mark the message as a RE-SEND into a successor
    /// one-to-one: `originGroupIdB64` is the base64 group id of the
    /// conversation it was originally sent in and `originMessageId` (a
    /// `bigint`) its DS `messageId` there. The marker is never rendered; it
    /// lets a device that already holds the original show the message once.
    /// Carried only when both halves are given (and the group id is
    /// non-empty and within its bound); otherwise any marker set earlier is
    /// cleared.
    #[wasm_bindgen(js_name = setResendOrigin)]
    pub fn set_resend_origin(
        &mut self,
        origin_group_id_b64: Option<String>,
        origin_message_id: Option<i64>,
    ) {
        self.resend = match (origin_group_id_b64, origin_message_id) {
            (Some(group_id_b64), Some(message_id)) => Some(inner::ResendOrigin {
                group_id_b64,
                message_id,
            }),
            _ => None,
        };
    }

    /// Append one attachment ref. Validates the byte-field lengths
    /// up front — wrong-length `blobId` / `key` / `nonce` throws
    /// immediately, not at build-time.
    #[wasm_bindgen]
    pub fn add(&mut self, r: &AttachmentRef) -> Result<(), JsError> {
        self.refs.push(ref_to_inner(r)?);
        Ok(())
    }

    /// Consume the builder + emit the JSON envelope bytes, with every
    /// optional part set on it (`setReply`, `setForward`,
    /// `setResendOrigin`). Hand the result to
    /// `Group.createApplicationMessage(identity, bytes)`.
    #[wasm_bindgen]
    pub fn build(self) -> Result<Vec<u8>, JsError> {
        inner::compose_chat(
            &self.text,
            self.refs,
            self.reply.as_ref(),
            self.forward.as_ref(),
            self.resend.as_ref(),
        )
        .map_err(attachment_err)
    }

    /// ADR-0052 — consume the builder + emit a reply envelope: the same
    /// chat message, also naming the DS `messageId` it answers (`replyTo`,
    /// a `bigint`) and carrying a snapshot of the quoted message as the
    /// replier saw it. The snapshot is what every receiver renders - later
    /// edits or deletion of the original never change the quote - and it
    /// is one level deep: replying to a reply quotes that reply's own text.
    /// Over-long snapshots are truncated on a character boundary; empty
    /// ones are not carried.
    #[wasm_bindgen(js_name = buildReply)]
    pub fn build_reply(
        mut self,
        reply_to: i64,
        reply_author: Option<String>,
        reply_excerpt: Option<String>,
    ) -> Result<Vec<u8>, JsError> {
        self.set_reply(reply_to, reply_author, reply_excerpt);
        self.build()
    }

    /// ADR-0052 — consume the builder + emit a FORWARDED message: the copied
    /// text and the re-encrypted attachments added with `add`, with the
    /// original author's name as a snapshot and, when both halves are given,
    /// the origin (base64 group id + `bigint` message id) for a recipient who
    /// is also in that conversation. A forward of a forward passes the
    /// ORIGINAL name and origin, never the forwarder's.
    #[wasm_bindgen(js_name = buildForward)]
    pub fn build_forward(
        mut self,
        forwarded_from: Option<String>,
        origin_group_id_b64: Option<String>,
        origin_message_id: Option<i64>,
    ) -> Result<Vec<u8>, JsError> {
        self.set_forward(forwarded_from, origin_group_id_b64, origin_message_id);
        self.build()
    }
}

/// The forward attribution for the given optional parts: the origin only as
/// a whole pair, and `None` when neither a name nor an origin is given.
fn forward_from_parts(
    forwarded_from: Option<String>,
    origin_group_id_b64: Option<String>,
    origin_message_id: Option<i64>,
) -> Option<inner::Forward> {
    let origin = match (origin_group_id_b64, origin_message_id) {
        (Some(group_id_b64), Some(message_id)) => Some(inner::ForwardOrigin {
            group_id_b64,
            message_id,
        }),
        _ => None,
    };
    (forwarded_from.is_some() || origin.is_some()).then_some(inner::Forward {
        author: forwarded_from,
        origin,
    })
}

/// Convenience for the no-attachment case: build a text-only
/// envelope without going through [`EnvelopeBuilder`].
#[wasm_bindgen(js_name = composeEnvelopeText)]
pub fn compose_envelope_text(text: &str) -> Result<Vec<u8>, JsError> {
    inner::compose(text, Vec::new()).map_err(attachment_err)
}

/// ADR-0052 — the text-only reply: [`EnvelopeBuilder::build_reply`]
/// without attachments.
#[wasm_bindgen(js_name = composeReplyEnvelopeText)]
pub fn compose_reply_envelope_text(
    text: &str,
    reply_to: i64,
    reply_author: Option<String>,
    reply_excerpt: Option<String>,
) -> Result<Vec<u8>, JsError> {
    let reply = inner::Reply {
        target: reply_to,
        author: reply_author,
        excerpt: reply_excerpt,
    };
    inner::compose_reply(text, Vec::new(), &reply).map_err(attachment_err)
}

/// Decode an MLS Application message JSON envelope. Mirror of
/// `lakko_attachments::parse` — never fails; legacy / malformed
/// inputs collapse into the documented fallback paths.
#[wasm_bindgen(js_name = parseEnvelope)]
#[must_use]
pub fn parse_envelope(bytes: &[u8]) -> ParsedEnvelope {
    let (text, refs) = inner::parse(bytes);
    // Same skip-malformed-silently posture as the UniFFI side. Receivers
    // see the rest of the attachments; the application path decides
    // whether a count mismatch is worth surfacing to the user.
    ParsedEnvelope {
        text,
        attachments: refs_to_wasm(refs),
    }
}

// ============================================================================
// ADR-0023 — edit / delete / reaction
// ============================================================================

/// Output of [`parse_body_envelope`] (ADR-0023). `kind` selects
/// which fields are meaningful: `text` + attachments for `"chat"`
/// (and `text` for `"edit"`); `target` for every op; `emoji` for
/// `"reaction"`. JS reads `target` as a `BigInt` (the DS `messageId`),
/// matching the `AttachmentRef.size` precedent.
#[wasm_bindgen]
pub struct ParsedBody {
    kind: &'static str,
    text: String,
    attachments: Vec<AttachmentRef>,
    target: Option<i64>,
    emoji: Option<String>,
    is_typing: Option<bool>,
    reply_to: Option<i64>,
    reply_author: Option<String>,
    reply_excerpt: Option<String>,
    forwarded_from: Option<String>,
    forward_origin_group_b64: Option<String>,
    forward_origin_message_id: Option<i64>,
    resend_origin_group_b64: Option<String>,
    resend_origin_message_id: Option<i64>,
    reaction_action: Option<&'static str>,
    reaction_target_author: Option<Vec<u8>>,
    channel_invite: Option<Vec<u8>>,
    content: Option<inner::Content>,
    poll_choices: Option<Vec<u8>>,
    checklist_item: Option<inner::ChecklistItemRef>,
    checklist_done: Option<bool>,
    poll_tally: Option<crate::channel_poll::ChannelPollTally>,
}

impl ParsedBody {
    const fn empty(kind: &'static str) -> Self {
        Self {
            kind,
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
            reaction_action: None,
            reaction_target_author: None,
            channel_invite: None,
            content: None,
            poll_choices: None,
            checklist_item: None,
            checklist_done: None,
            poll_tally: None,
        }
    }

    fn with_reply_forward(
        mut self,
        reply: Option<inner::Reply>,
        forward: Option<inner::Forward>,
    ) -> Self {
        self.reply_to = reply.as_ref().map(|r| r.target);
        self.reply_author = reply.as_ref().and_then(|r| r.author.clone());
        self.reply_excerpt = reply.and_then(|r| r.excerpt);
        self.forwarded_from = forward.as_ref().and_then(|f| f.author.clone());
        self.forward_origin_group_b64 = forward
            .as_ref()
            .and_then(|f| f.origin.as_ref().map(|o| o.group_id_b64.clone()));
        self.forward_origin_message_id = forward.and_then(|f| f.origin.map(|o| o.message_id));
        self
    }

    fn with_content(mut self, content: inner::Content) -> Self {
        self.content = Some(content);
        self
    }
}

#[wasm_bindgen]
impl ParsedBody {
    /// ADR-0090 — the `MessageContent` object of a `"poll"` / `"checklist"`
    /// / `"location"` / `"contact-card"` / `"article"` message, or the
    /// replacement of an `"edit-checklist"` / `"edit-article"`; otherwise
    /// `undefined`. Byte fields inside are standard base64.
    #[wasm_bindgen(getter, unchecked_return_type = "MessageContent | undefined")]
    pub fn content(&self) -> JsValue {
        self.content
            .as_ref()
            .map_or(JsValue::UNDEFINED, crate::content::content_to_js)
    }

    /// ADR-0090 — a `"poll-vote"`'s choices (empty = retract).
    #[wasm_bindgen(getter, js_name = pollChoices)]
    pub fn poll_choices(&self) -> Option<Vec<u8>> {
        self.poll_choices.clone()
    }

    /// ADR-0090 — the `ChecklistItemRef` a `"checklist-tick"` names.
    #[wasm_bindgen(getter, js_name = checklistItem, unchecked_return_type = "ChecklistItemRef | undefined")]
    pub fn checklist_item(&self) -> JsValue {
        self.checklist_item.map_or(JsValue::UNDEFINED, |i| {
            crate::content::to_js(&crate::content::WItemRef::from(i)).unwrap_or(JsValue::UNDEFINED)
        })
    }

    /// ADR-0090 section 4.3 — a channel poll's frozen tally on a
    /// `"poll-close"`, decoded; `undefined` in a group, or when absent or not
    /// decodable (the close stands).
    #[wasm_bindgen(getter, js_name = pollTally)]
    pub fn poll_tally(&self) -> Option<crate::channel_poll::ChannelPollTally> {
        self.poll_tally.clone()
    }

    /// ADR-0090 — a `"checklist-tick"`'s done state.
    #[wasm_bindgen(getter, js_name = checklistDone)]
    pub fn checklist_done(&self) -> Option<bool> {
        self.checklist_done
    }

    /// UX reactions-and-emoji section 17: `"add"` or `"remove"`, set only
    /// on `kind == "reaction"`.
    #[wasm_bindgen(getter, js_name = reactionAction)]
    pub fn reaction_action(&self) -> Option<String> {
        self.reaction_action.map(str::to_string)
    }

    /// UX reactions-and-emoji section 17: the 32-byte account key of the
    /// reacted-to message's author as the reactor saw it, or `undefined`.
    /// The reactor's claim, never an identity.
    #[wasm_bindgen(getter, js_name = reactionTargetAuthor)]
    pub fn reaction_target_author(&self) -> Option<Vec<u8>> {
        self.reaction_target_author.clone()
    }

    /// ADR-0087 section 14: a channel invitation card beside the text, set
    /// only on `kind == "chat"`. Open it with `openChannelInviteCard`.
    #[wasm_bindgen(getter, js_name = channelInvite)]
    pub fn channel_invite(&self) -> Option<Vec<u8>> {
        self.channel_invite.clone()
    }

    /// ADR-0052 — on a forwarded message, the ORIGINAL author's display name
    /// as a snapshot: a label, never a verified identity or a route to them.
    #[wasm_bindgen(getter, js_name = forwardedFrom)]
    pub fn forwarded_from(&self) -> Option<String> {
        self.forwarded_from.clone()
    }

    /// ADR-0052 — base64 of the origin conversation's group id, set only
    /// together with `forwardOriginMessageId`; for a local jump by a recipient
    /// who is also in that conversation.
    #[wasm_bindgen(getter, js_name = forwardOriginGroupB64)]
    pub fn forward_origin_group_b64(&self) -> Option<String> {
        self.forward_origin_group_b64.clone()
    }

    /// ADR-0052 — the DS `messageId` of the original (a `bigint`).
    #[wasm_bindgen(getter, js_name = forwardOriginMessageId)]
    pub fn forward_origin_message_id(&self) -> Option<i64> {
        self.forward_origin_message_id
    }

    /// ADR-0073 — on a message re-sent into a successor one-to-one, base64
    /// of the ORIGINAL conversation's group id; set only together with
    /// `resendOriginMessageId`. Never shown: it lets a device that already
    /// holds the original show the message once.
    #[wasm_bindgen(getter, js_name = resendOriginGroupB64)]
    pub fn resend_origin_group_b64(&self) -> Option<String> {
        self.resend_origin_group_b64.clone()
    }

    /// ADR-0073 — the DS `messageId` of the original in that conversation
    /// (a `bigint`).
    #[wasm_bindgen(getter, js_name = resendOriginMessageId)]
    pub fn resend_origin_message_id(&self) -> Option<i64> {
        self.resend_origin_message_id
    }

    /// ADR-0052 — the DS `messageId` this message replies to, or `undefined`
    /// (a `bigint`, like `target`). Only ever set on `kind == "chat"`.
    #[wasm_bindgen(getter, js_name = replyTo)]
    pub fn reply_to(&self) -> Option<i64> {
        self.reply_to
    }

    /// ADR-0052 — the quoted author's name as the replier wrote it: a
    /// snapshot and a courtesy label, never a verified identity.
    #[wasm_bindgen(getter, js_name = replyAuthor)]
    pub fn reply_author(&self) -> Option<String> {
        self.reply_author.clone()
    }

    /// ADR-0052 — an excerpt of the quoted message's own text, one level
    /// deep, unchanged by later edits or deletion of the original.
    #[wasm_bindgen(getter, js_name = replyExcerpt)]
    pub fn reply_excerpt(&self) -> Option<String> {
        self.reply_excerpt.clone()
    }

    /// `"chat" | "edit" | "delete" | "reaction" | "typing" | "read-receipt" |
    /// "service-envelope" | "unsupported"`, and since ADR-0090 `"poll" |
    /// "checklist" | "location" | "contact-card" | "article"` (the value in
    /// `content`), `"poll-vote" | "poll-close" | "checklist-tick" |
    /// "checklist-add"` (ops on `target`, never a bubble) and
    /// `"edit-checklist" | "edit-article"` (the replacement in `content`). `"service-envelope"` is never
    /// rendered; `"unsupported"` (ADR-0061 section 6) is a chat envelope from
    /// a newer wire version, rendered with the client's own catalog sentence
    /// (its `text` is empty — no core prose).
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.to_string()
    }

    /// The typing state, or `undefined` unless `kind == "typing"` (ADR-0024).
    #[wasm_bindgen(getter, js_name = isTyping)]
    pub fn is_typing(&self) -> Option<bool> {
        self.is_typing
    }

    /// Chat / edit text (empty for delete / reaction).
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text.clone()
    }

    /// The DS `messageId` the op acts on, or `undefined` for `"chat"`. For
    /// `"unsupported"` it carries the envelope's wire version, as on UniFFI.
    #[wasm_bindgen(getter)]
    pub fn target(&self) -> Option<i64> {
        self.target
    }

    /// ADR-0061 section 6 — the wire version of an `"unsupported"` envelope,
    /// or `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = unsupportedVersion)]
    pub fn unsupported_version(&self) -> Option<u8> {
        if self.kind == "unsupported" {
            self.target.and_then(|v| u8::try_from(v).ok())
        } else {
            None
        }
    }

    /// The reaction emoji, or `undefined` unless `kind == "reaction"`.
    #[wasm_bindgen(getter)]
    pub fn emoji(&self) -> Option<String> {
        self.emoji.clone()
    }

    /// Number of attachments (chat only; `0` otherwise).
    #[wasm_bindgen(getter, js_name = attachmentCount)]
    pub fn attachment_count(&self) -> u32 {
        u32::try_from(self.attachments.len()).unwrap_or(u32::MAX)
    }

    /// Pull one attachment by index (chat only).
    ///
    /// # Errors
    ///
    /// `RangeError` on out-of-bounds `i`.
    #[wasm_bindgen]
    pub fn attachment(&self, i: u32) -> Result<AttachmentRef, JsError> {
        let idx = usize::try_from(i).map_err(|_| boundary_err(format!("bad index {i}")))?;
        self.attachments
            .get(idx)
            .cloned()
            .ok_or_else(|| boundary_err(format!("attachment index {i} out of bounds")))
    }
}

/// Build an EDIT envelope (ADR-0023) replacing the target message's
/// body with `new_text`. `target` is the DS `messageId` (`BigInt`).
///
/// # Errors
///
/// `Error` on serialisation failure.
#[wasm_bindgen(js_name = composeEditEnvelope)]
pub fn compose_edit_envelope(target: i64, new_text: &str) -> Result<Vec<u8>, JsError> {
    inner::compose_edit(target, new_text).map_err(attachment_err)
}

/// Build a DELETE (tombstone) envelope (ADR-0023) for `target`.
///
/// # Errors
///
/// `Error` on serialisation failure.
#[wasm_bindgen(js_name = composeDeleteEnvelope)]
pub fn compose_delete_envelope(target: i64) -> Result<Vec<u8>, JsError> {
    inner::compose_delete(target).map_err(attachment_err)
}

/// Build a REACTION envelope (ADR-0023): `emoji` to `target`.
///
/// # Errors
///
/// `Error` if `emoji` is empty or exceeds `MAX_EMOJI_BYTES`, or on
/// serialisation failure.
///
/// `action` is `"add"` or `"remove"` (UX reactions-and-emoji section 17);
/// `targetAuthor` the 32-byte account key of the reacted-to message's author
/// as this device resolved it, or `undefined`.
#[wasm_bindgen(js_name = composeReactionEnvelope)]
pub fn compose_reaction_envelope(
    target: i64,
    emoji: &str,
    action: &str,
    target_author: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let action = match action {
        "add" => inner::ReactionAction::Add,
        "remove" => inner::ReactionAction::Remove,
        _ => return Err("reaction.badAction: action must be \"add\" or \"remove\"".to_string()),
    };
    let author = target_author
        .map(|k| {
            <[u8; inner::REACTION_TARGET_AUTHOR_LEN]>::try_from(k.as_slice()).map_err(|_| {
                format!(
                    "reaction.badTargetAuthor: target author must be {} bytes, got {}",
                    inner::REACTION_TARGET_AUTHOR_LEN,
                    k.len()
                )
            })
        })
        .transpose()?;
    inner::compose_reaction(target, emoji, action, author.as_ref())
        .map_err(|e| format!("reaction.invalid: {e}"))
}

/// Build a chat envelope carrying a channel invitation card (ADR-0087
/// section 14) beside `text` (may be empty); the card comes from
/// `composeChannelInviteCard`.
///
/// # Errors
///
/// Throws for an empty card or one longer than `maxChannelInviteBytes()`.
#[wasm_bindgen(js_name = composeChannelInviteEnvelope)]
pub fn compose_channel_invite_envelope(text: &str, card: &[u8]) -> Result<Vec<u8>, String> {
    inner::compose_channel_invite(text, card).map_err(|e| format!("channelInvite.invalid: {e}"))
}

/// The longest channel invitation card the chat envelope carries.
#[wasm_bindgen(js_name = maxChannelInviteBytes)]
#[must_use]
pub fn max_channel_invite_bytes() -> u32 {
    u32::try_from(inner::MAX_CHANNEL_INVITE_BYTES).unwrap_or(u32::MAX)
}

/// Build an ephemeral TYPING envelope (ADR-0024).
///
/// # Errors
///
/// `Error` on serialisation failure.
#[wasm_bindgen(js_name = composeTypingEnvelope)]
pub fn compose_typing_envelope(is_typing: bool) -> Result<Vec<u8>, JsError> {
    inner::compose_typing(is_typing).map_err(attachment_err)
}

/// Build a READ RECEIPT envelope (ADR-0024): read up to `up_to`.
///
/// # Errors
///
/// `Error` on serialisation failure.
#[wasm_bindgen(js_name = composeReadReceiptEnvelope)]
pub fn compose_read_receipt_envelope(up_to: i64) -> Result<Vec<u8>, JsError> {
    inner::compose_read_receipt(up_to).map_err(attachment_err)
}

/// Decode an MLS Application message body into a classified
/// [`ParsedBody`] (ADR-0023/0024). Never fails — a malformed /
/// ambiguous op degrades to `"chat"`.
#[wasm_bindgen(js_name = parseBodyEnvelope)]
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per body kind
pub fn parse_body_envelope(bytes: &[u8]) -> ParsedBody {
    use inner::MessageBody as B;
    match inner::parse_body(bytes) {
        B::Chat {
            text,
            attachments,
            reply,
            forward,
            resend,
            channel_invite,
        } => {
            let mut p = ParsedBody::empty("chat").with_reply_forward(reply, forward);
            p.text = text;
            p.attachments = refs_to_wasm(attachments);
            p.resend_origin_group_b64 = resend.as_ref().map(|r| r.group_id_b64.clone());
            p.resend_origin_message_id = resend.map(|r| r.message_id);
            p.channel_invite = channel_invite;
            p
        },
        B::Edit { target, text } => {
            let mut p = ParsedBody::empty("edit");
            p.text = text;
            p.target = Some(target);
            p
        },
        B::Delete { target } => {
            let mut p = ParsedBody::empty("delete");
            p.target = Some(target);
            p
        },
        B::Reaction {
            target,
            emoji,
            action,
            target_author,
        } => {
            let mut p = ParsedBody::empty("reaction");
            p.target = Some(target);
            p.emoji = Some(emoji);
            p.reaction_action = Some(match action {
                inner::ReactionAction::Add => "add",
                inner::ReactionAction::Remove => "remove",
            });
            p.reaction_target_author = target_author.map(|k| k.to_vec());
            p
        },
        B::Typing { is_typing } => {
            let mut p = ParsedBody::empty("typing");
            p.is_typing = Some(is_typing);
            p
        },
        B::ReadReceipt { up_to } => {
            let mut p = ParsedBody::empty("read-receipt");
            p.target = Some(up_to);
            p
        },
        // ADR-0061 section 6: a newer wire version; the client renders its
        // own sentence. The version rides `target`, as on UniFFI.
        B::Unsupported { version } => {
            let mut p = ParsedBody::empty("unsupported");
            p.target = Some(i64::from(version));
            p
        },
        // A Lakko service envelope this build did not recognise: never render.
        B::ServiceEnvelope => ParsedBody::empty("service-envelope"),
        B::Poll {
            poll,
            reply,
            forward,
        } => ParsedBody::empty("poll")
            .with_reply_forward(reply, forward)
            .with_content(inner::Content::Poll(poll)),
        B::Checklist {
            checklist,
            reply,
            forward,
        } => ParsedBody::empty("checklist")
            .with_reply_forward(reply, forward)
            .with_content(inner::Content::Checklist(checklist)),
        B::Location {
            location,
            reply,
            forward,
        } => ParsedBody::empty("location")
            .with_reply_forward(reply, forward)
            .with_content(inner::Content::Location(location)),
        B::ContactCard {
            card,
            reply,
            forward,
        } => ParsedBody::empty("contact-card")
            .with_reply_forward(reply, forward)
            .with_content(inner::Content::ContactCard(card)),
        B::Article {
            article,
            attachments,
            reply,
            forward,
        } => {
            let mut p = ParsedBody::empty("article")
                .with_reply_forward(reply, forward)
                .with_content(inner::Content::Article(article));
            p.attachments = refs_to_wasm(attachments);
            p
        },
        B::PollVote { target, choices } => {
            let mut p = ParsedBody::empty("poll-vote");
            p.target = Some(target);
            p.poll_choices = Some(choices);
            p
        },
        B::PollClose { target, tally } => {
            let mut p = ParsedBody::empty("poll-close");
            p.target = Some(target);
            p.poll_tally = tally.and_then(|t| crate::channel_poll::decode_close_tally(&t));
            p
        },
        B::ChecklistTick { target, item, done } => {
            let mut p = ParsedBody::empty("checklist-tick");
            p.target = Some(target);
            p.checklist_item = Some(item);
            p.checklist_done = Some(done);
            p
        },
        B::ChecklistAdd { target, text } => {
            let mut p = ParsedBody::empty("checklist-add");
            p.target = Some(target);
            p.text = text;
            p
        },
        B::EditChecklist { target, checklist } => {
            let mut p = ParsedBody::empty("edit-checklist")
                .with_content(inner::Content::Checklist(checklist));
            p.target = Some(target);
            p
        },
        B::EditArticle {
            target,
            article,
            attachments,
        } => {
            let mut p =
                ParsedBody::empty("edit-article").with_content(inner::Content::Article(article));
            p.target = Some(target);
            p.attachments = refs_to_wasm(attachments);
            p
        },
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Decode inner `AttachmentRef`s into the wasm shape, dropping any
/// malformed ref silently (the same policy as [`parse_envelope`]).
fn refs_to_wasm(refs: Vec<inner::AttachmentRef>) -> Vec<AttachmentRef> {
    refs.into_iter()
        .filter_map(|r| {
            let blob_id = r.decode_blob_id().ok()?;
            let key = r.decode_key().ok()?;
            let nonce = r.decode_nonce().ok()?;
            // ADR-0047 media fields arrive already bounded by the core
            // parser; a thumb that still fails to decode is dropped alone.
            let waveform = r.decode_waveform();
            let thumb = r.thumb.as_ref().and_then(thumb_to_wasm);
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
                title: r.title,
                artist: r.artist,
            })
        })
        .collect()
}

fn thumb_to_wasm(t: &inner::ThumbRef) -> Option<ThumbRef> {
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

/// Convert a wasm ref to the core wire ref, refusing wrong-length raw
/// byte fields (a caller bug). The ADR-0047 bounds on media fields are
/// applied later by `inner::compose`, which drops rather than refuses.
fn ref_to_inner(r: &AttachmentRef) -> Result<inner::AttachmentRef, JsError> {
    check_byte_fields("", &r.blob_id, &r.key, &r.nonce)?;
    let thumb = match &r.thumb {
        None => None,
        Some(t) => {
            check_byte_fields("thumb ", &t.blob_id, &t.key, &t.nonce)?;
            Some(inner::ThumbRef {
                blob_id: B64.encode(&t.blob_id),
                key: B64.encode(&t.key),
                nonce: B64.encode(&t.nonce),
                mime: t.mime.clone(),
                size: t.size,
                width: t.width,
                height: t.height,
            })
        },
    };
    Ok(inner::AttachmentRef {
        blob_id: B64.encode(&r.blob_id),
        key: B64.encode(&r.key),
        nonce: B64.encode(&r.nonce),
        mime: r.mime.clone(),
        size: r.size,
        name: r.name.clone(),
        kind: r.kind.clone(),
        duration_ms: r.duration_ms,
        waveform: r.waveform.as_ref().map(|w| B64.encode(w)),
        width: r.width,
        height: r.height,
        thumb,
        title: r.title.clone(),
        artist: r.artist.clone(),
    })
}

/// Convert wasm refs to core wire refs (the content composers' input).
pub(crate) fn refs_to_inner(refs: &[AttachmentRef]) -> Result<Vec<inner::AttachmentRef>, JsError> {
    refs.iter().map(ref_to_inner).collect()
}

fn check_byte_fields(
    prefix: &str,
    blob_id: &[u8],
    key: &[u8],
    nonce: &[u8],
) -> Result<(), JsError> {
    for (field, got, want) in [
        ("blob_id", blob_id.len(), inner::BLOB_ID_LEN),
        ("key", key.len(), inner::ATTACHMENT_KEY_LEN),
        ("nonce", nonce.len(), inner::ATTACHMENT_NONCE_LEN),
    ] {
        if got != want {
            return Err(boundary_err(format!(
                "{prefix}{field} length {got} != {want}"
            )));
        }
    }
    Ok(())
}

fn bytes_to_aead_key(bytes: &[u8]) -> Result<AeadKey, JsError> {
    if bytes.len() != inner::ATTACHMENT_KEY_LEN {
        return Err(boundary_err(format!(
            "key length {} != {}",
            bytes.len(),
            inner::ATTACHMENT_KEY_LEN
        )));
    }
    let mut arr = [0u8; inner::ATTACHMENT_KEY_LEN];
    arr.copy_from_slice(bytes);
    Ok(AeadKey::new(arr))
}

fn bytes_to_nonce(bytes: &[u8]) -> Result<[u8; inner::ATTACHMENT_NONCE_LEN], JsError> {
    if bytes.len() != inner::ATTACHMENT_NONCE_LEN {
        return Err(boundary_err(format!(
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
    fn encrypt_then_decrypt_round_trip() {
        let plaintext = b"hello attachments via wasm";
        let enc = encrypt_attachment(plaintext).unwrap();
        assert_eq!(enc.ciphertext().len(), plaintext.len() + 16);
        let recovered = decrypt_attachment(&enc.ciphertext(), &enc.key(), &enc.nonce()).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn compose_envelope_text_then_parse_round_trip() {
        let bytes = compose_envelope_text("hi there").unwrap();
        let parsed = parse_envelope(&bytes);
        assert_eq!(parsed.text(), "hi there");
        assert_eq!(parsed.attachment_count(), 0);
    }

    #[test]
    fn builder_round_trip_with_one_attachment() {
        let enc = encrypt_attachment(b"image bytes").unwrap();
        let r = AttachmentRef::new(
            enc.blob_id(),
            enc.key(),
            enc.nonce(),
            Some("image/png".to_string()),
            11,
            Some("cat.png".to_string()),
        );
        let mut builder = EnvelopeBuilder::new("look at this".to_string());
        builder.add(&r).unwrap();
        let bytes = builder.build().unwrap();

        let parsed = parse_envelope(&bytes);
        assert_eq!(parsed.text(), "look at this");
        assert_eq!(parsed.attachment_count(), 1);
        let got = parsed.attachment(0).unwrap();
        assert_eq!(got.blob_id(), r.blob_id());
        assert_eq!(got.key(), r.key());
        assert_eq!(got.nonce(), r.nonce());
        assert_eq!(got.mime(), Some("image/png".to_string()));
        assert_eq!(got.size(), 11);
        assert_eq!(got.name(), Some("cat.png".to_string()));
    }

    // Error-path tests (oversize plaintext, wrong-length keys,
    // out-of-bounds index, etc.) live in `lakko-attachments`'s
    // host-side unit tests + the apps/web `wasm-bindgen-test`
    // suite. Calling `JsError::new` on a non-wasm host target
    // panics with "cannot call wasm-bindgen imported functions on
    // non-wasm targets" — same posture documented for the rest of
    // the ffi-wasm error paths.

    #[test]
    fn parse_legacy_bare_utf8_falls_back() {
        let parsed = parse_envelope(b"plain text");
        assert_eq!(parsed.text(), "plain text");
        assert_eq!(parsed.attachment_count(), 0);
    }

    #[test]
    fn newer_wire_version_body_is_unsupported_with_no_text() {
        let body = parse_body_envelope(br#"{"v":7,"text":"future"}"#);
        assert_eq!(body.kind(), "unsupported");
        assert_eq!(body.unsupported_version(), Some(7));
        assert_eq!(body.target(), Some(7));
        assert!(body.text().is_empty());
        assert_eq!(body.attachment_count(), 0);
        let chat = parse_body_envelope(br#"{"v":1,"text":"hi"}"#);
        assert_eq!(chat.unsupported_version(), None);
    }

    #[test]
    fn parse_unknown_version_surfaces_placeholder() {
        let evil = br#"{"v":99,"text":"future","attachments":[]}"#;
        let parsed = parse_envelope(evil);
        assert_eq!(parsed.text(), "[unsupported v99 message]");
        assert_eq!(parsed.attachment_count(), 0);
    }

    // ---- ADR-0047 ----

    #[test]
    fn builder_round_trips_media_metadata_through_both_parsers() {
        let enc = encrypt_attachment(b"note bytes").unwrap();
        let poster = encrypt_attachment(b"poster").unwrap();
        let thumb = ThumbRef::new(
            poster.blob_id(),
            poster.key(),
            poster.nonce(),
            Some("image/jpeg".to_string()),
            6,
            Some(240),
            Some(240),
        );
        let mut r = AttachmentRef::new(
            enc.blob_id(),
            enc.key(),
            enc.nonce(),
            Some("video/mp4".to_string()),
            10,
            Some("note-1.mp4".to_string()),
        );
        r.set_kind(Some("video_note".to_string()));
        r.set_duration_ms(Some(9000));
        r.set_waveform(Some(vec![4, 5, 6]));
        r.set_width(Some(480));
        r.set_height(Some(480));
        r.set_thumb(&thumb);
        // The caller's thumb handle is still usable after attaching it.
        assert_eq!(thumb.size(), 6);

        let mut builder = EnvelopeBuilder::new("c".to_string());
        builder.add(&r).unwrap();
        let bytes = builder.build().unwrap();
        for got in [
            parse_envelope(&bytes).attachment(0).unwrap(),
            parse_body_envelope(&bytes).attachment(0).unwrap(),
        ] {
            assert_eq!(got.kind(), Some("video_note".to_string()));
            assert_eq!(got.duration_ms(), Some(9000));
            assert_eq!(got.waveform(), Some(vec![4, 5, 6]));
            assert_eq!((got.width(), got.height()), (Some(480), Some(480)));
            let t = got.thumb().unwrap();
            assert_eq!(
                (t.blob_id(), t.key(), t.nonce()),
                (thumb.blob_id(), thumb.key(), thumb.nonce())
            );
            assert_eq!((t.size(), t.width(), t.height()), (6, Some(240), Some(240)));
            assert_eq!(t.mime(), Some("image/jpeg".to_string()));
        }
    }

    #[test]
    fn out_of_bound_media_metadata_is_dropped_at_build() {
        let enc = encrypt_attachment(b"voice").unwrap();
        let mut r = AttachmentRef::new(enc.blob_id(), enc.key(), enc.nonce(), None, 5, None);
        r.set_kind(Some("voice".to_string()));
        r.set_duration_ms(Some(15 * 60 * 1000 + 1));
        r.set_waveform(Some(vec![0u8; 129]));
        r.set_width(Some(0));
        r.set_height(Some(4096));
        let mut builder = EnvelopeBuilder::new("c".to_string());
        builder.add(&r).unwrap();
        let got = parse_envelope(&builder.build().unwrap())
            .attachment(0)
            .unwrap();
        assert_eq!(got.kind(), Some("voice".to_string()));
        assert_eq!((got.duration_ms(), got.waveform()), (None, None));
        assert_eq!((got.width(), got.height()), (None, Some(4096)));
        assert!(got.thumb().is_none());
    }

    #[test]
    fn inline_media_type_follows_the_core_allowlist() {
        assert_eq!(
            inline_media_type("audio/mp4;codecs=mp4a.40.2"),
            Some(InlineMediaType::Audio)
        );
        assert_eq!(inline_media_type("video/mp4"), Some(InlineMediaType::Video));
        assert_eq!(
            inline_media_type("image/webp"),
            Some(InlineMediaType::Image)
        );
        assert_eq!(inline_media_type("image/svg+xml"), None);
    }

    #[test]
    fn reply_round_trips_through_builder_and_text_helper() {
        let enc = encrypt_attachment(b"voice bytes").unwrap();
        let r = AttachmentRef::new(
            enc.blob_id(),
            enc.key(),
            enc.nonce(),
            Some("audio/mp4".to_string()),
            11,
            None,
        );
        let mut b = EnvelopeBuilder::new("sure".to_string());
        b.add(&r).unwrap();
        let wire = b
            .build_reply(41, Some("Alice".to_string()), Some("lunch?".to_string()))
            .unwrap();
        let body = parse_body_envelope(&wire);
        assert_eq!(body.kind(), "chat");
        assert_eq!(body.reply_to(), Some(41));
        assert_eq!(body.reply_author().as_deref(), Some("Alice"));
        assert_eq!(body.reply_excerpt().as_deref(), Some("lunch?"));
        assert_eq!(body.target(), None);
        assert_eq!(body.attachment_count(), 1);

        let wire = compose_reply_envelope_text("ok", 7, None, None).unwrap();
        let body = parse_body_envelope(&wire);
        assert_eq!(body.reply_to(), Some(7));
        assert_eq!(body.reply_author(), None);
        assert_eq!(body.reply_excerpt(), None);

        let body = parse_body_envelope(&compose_envelope_text("hi").unwrap());
        assert_eq!(body.reply_to(), None);
    }

    #[test]
    fn resend_marker_round_trips_through_the_builder() {
        let mut b = EnvelopeBuilder::new("again".to_string());
        b.set_reply(5, Some("Bob".to_string()), None);
        b.set_forward(Some("Alice".to_string()), None, None);
        b.set_resend_origin(Some("b2xk".to_string()), Some(17));
        let body = parse_body_envelope(&b.build().unwrap());
        assert_eq!(body.kind(), "chat");
        assert_eq!(body.reply_to(), Some(5));
        assert_eq!(body.forwarded_from().as_deref(), Some("Alice"));
        assert_eq!(body.resend_origin_group_b64().as_deref(), Some("b2xk"));
        assert_eq!(body.resend_origin_message_id(), Some(17));
        // The marker survives the older build methods too.
        let mut b = EnvelopeBuilder::new("again".to_string());
        b.set_resend_origin(Some("b2xk".to_string()), Some(17));
        let body = parse_body_envelope(&b.build_reply(5, None, None).unwrap());
        assert_eq!(body.reply_to(), Some(5));
        assert_eq!(body.resend_origin_message_id(), Some(17));
        // Half a marker is no marker, and a plain build carries none.
        let mut b = EnvelopeBuilder::new("x".to_string());
        b.set_resend_origin(Some("b2xk".to_string()), None);
        let wire = b.build().unwrap();
        assert_eq!(wire, compose_envelope_text("x").unwrap());
        let body = parse_body_envelope(&wire);
        assert_eq!(body.resend_origin_group_b64(), None);
        assert_eq!(body.resend_origin_message_id(), None);
        // An op that also carries the marker is still that op.
        let body = parse_body_envelope(
            br#"{"v":1,"delete_of":7,"resend_origin_group":"b2xk","resend_origin_message":3}"#,
        );
        assert_eq!(body.kind(), "delete");
        assert_eq!(body.resend_origin_message_id(), None);
    }

    #[test]
    fn forward_round_trips_through_the_builder() {
        let b = EnvelopeBuilder::new("copied".to_string());
        let wire = b
            .build_forward(
                Some("Alice".to_string()),
                Some("Z3JvdXA=".to_string()),
                Some(41),
            )
            .unwrap();
        let body = parse_body_envelope(&wire);
        assert_eq!(body.kind(), "chat");
        assert_eq!(body.forwarded_from().as_deref(), Some("Alice"));
        assert_eq!(body.forward_origin_group_b64().as_deref(), Some("Z3JvdXA="));
        assert_eq!(body.forward_origin_message_id(), Some(41));
        assert_eq!(body.reply_to(), None);
        let body = parse_body_envelope(&compose_envelope_text("hi").unwrap());
        assert_eq!(body.forwarded_from(), None);
        assert_eq!(body.forward_origin_message_id(), None);
    }

    #[test]
    fn reaction_op_and_channel_invite_cross_the_boundary() {
        let bytes = compose_reaction_envelope(9, "\u{1F44D}", "remove", Some(vec![4; 32])).unwrap();
        let body = parse_body_envelope(&bytes);
        assert_eq!(body.kind(), "reaction");
        assert_eq!(body.reaction_action(), Some("remove".to_string()));
        assert_eq!(body.reaction_target_author(), Some(vec![4; 32]));
        assert!(compose_reaction_envelope(9, "x", "toggle", None).is_err());
        assert!(compose_reaction_envelope(9, "x", "add", Some(vec![4; 31])).is_err());
        let card = vec![0x5A; 300];
        let body = parse_body_envelope(&compose_channel_invite_envelope("Join", &card).unwrap());
        assert_eq!(body.kind(), "chat");
        assert_eq!(body.channel_invite(), Some(card));
    }
}
