// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-attachment encryption + the application-message JSON wire
//! envelope that carries attachment references (Phase 1.19a).
//!
//! # Threat model
//!
//! `lakko-svc-storage` (Phase 1.2) is a content-addressed blob proxy
//! keyed by `SHA-256(ciphertext)`. The server is **blind** — it sees
//! only opaque ciphertext + the SHA-256 used as the lookup key. The
//! key-encryption-key (KEK) that decrypts a given blob never crosses
//! the storage wire; it travels inside the MLS Application message
//! that references the blob, so only group members can decrypt.
//!
//! For every attachment the sender:
//!
//! 1. Generates a fresh random 32-byte CEK + 12-byte nonce.
//! 2. AEAD-encrypts the plaintext under the CEK (ChaCha20-Poly1305,
//!    AAD = empty — the SHA-256 below binds the ciphertext to the
//!    blob_id authenticator, so AAD adds nothing).
//! 3. Computes `blob_id = SHA-256(ciphertext)`.
//! 4. PUTs the ciphertext to `/blob/<hex(blob_id)>` on the storage
//!    service.
//! 5. Emits an MLS Application message whose plaintext is the JSON
//!    envelope (see [`compose`]) carrying the user-visible text +
//!    one [`AttachmentRef`] per attached file.
//!
//! Receivers parse the envelope, GET each blob, AEAD-decrypt with
//! the CEK from the ref, and surface the plaintext bytes to the UI
//! layer (image renderer, file save, etc.).
//!
//! # Why per-attachment fresh keys (not derived from MLS exporter)
//!
//! An MLS-exporter-derived key would tie the attachment to the
//! group's current epoch — but blobs are content-addressed and we
//! want one upload to be readable by every future member of the
//! group (including members added after the message was sent, via
//! re-share of the same message reference). A standalone CEK per
//! attachment matches the message-key paradigm of the existing
//! 1.18a InviteCapability + 1×1 contact-request envelope (ADR-0005
//! §13).
//!
//! # Wire envelope versioning
//!
//! The application-message plaintext is a JSON object. v1 is
//! [`Envelope`] with a `v` tag = `1` and two fields (`text` +
//! `attachments`). Receivers MUST tolerate unknown fields (forward-
//! compat with v1.x additions like `reply_to`, `edit_of`, …) and
//! MUST fall back to treating the raw bytes as bare UTF-8 text on
//! JSON parse failure (legacy / test-harness compat — pre-1.19
//! chat messages are bare UTF-8 with no envelope).
//!
//! # Media metadata (ADR-0047)
//!
//! Voice messages, video notes and GIFs are ordinary attachments with
//! optional, bounded metadata on the [`AttachmentRef`]: `kind`,
//! `duration_ms`, `waveform`, `width`, `height`, `thumb`. Every one of
//! them is sender-chosen and therefore attacker-controlled, so:
//!
//! - **A field that fails its bound is dropped; the attachment and the
//!   envelope survive.** That includes a field of the wrong JSON type
//!   (`"duration_ms": "abc"`): an envelope that fails to parse falls
//!   back to rendering its raw bytes as text, which would show a voice
//!   message to the user as JSON. The same sanitizer runs in
//!   [`compose`], so what goes on the wire is what every receiver keeps.
//! - `kind` is an open string, never a closed enum on the wire — an
//!   unknown future kind still decodes (see [`MediaKind::Other`]).
//! - **Inline playback is decided by MIME type alone**
//!   ([`inline_media_type`]), never by `kind`. `kind: voice` with
//!   `mime: text/html` is a file.
//! - [`WIRE_VERSION`] stays 1.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use lakko_crypto::aead::{open, seal, AeadKey, Algorithm, KEY_LEN, NONCE_LEN};

/// Length of a content-addressed blob id (= SHA-256 output).
pub const BLOB_ID_LEN: usize = 32;

/// Length of a per-attachment AEAD key.
pub use lakko_crypto::aead::KEY_LEN as ATTACHMENT_KEY_LEN;

/// Length of an AEAD nonce.
pub use lakko_crypto::aead::NONCE_LEN as ATTACHMENT_NONCE_LEN;

/// Hard ceiling on per-attachment plaintext size — mirrors the
/// 50 MB ceiling `services/storage` enforces on each blob PUT
/// (ciphertext is plaintext + 16-byte AEAD tag — within rounding,
/// the limit applies to either side). Clients should surface a
/// clear "file too large" error rather than letting the server
/// reject the upload.
pub const MAX_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;

/// Wire envelope version. Bump in lockstep with breaking changes
/// to [`Envelope`]; current shape carries `v = 1`. Edit / delete /
/// reaction (ADR-0023) ride as `#[serde(default)]` v1.x fields — a
/// v1.0 parser tolerates them as unknown, so no version bump.
pub const WIRE_VERSION: u8 = 1;

/// Hard ceiling on a reaction emoji string (ADR-0023). Generous
/// enough for a ZWJ grapheme cluster (family / flag / skin-tone
/// sequences run ~25-30 bytes) while bounding the attacker-touchable
/// parser — anything longer degrades to a plain chat message.
pub const MAX_EMOJI_BYTES: usize = 64;

/// ADR-0047 — the wire value of [`AttachmentRef::kind`] for a voice
/// message.
pub const MEDIA_KIND_VOICE: &str = "voice";

/// ADR-0047 — the wire value of [`AttachmentRef::kind`] for a round
/// video note.
pub const MEDIA_KIND_VIDEO_NOTE: &str = "video_note";

/// ADR-0047 — the wire value of [`AttachmentRef::kind`] for a GIF
/// picked from the GIF proxy and re-uploaded as an attachment.
pub const MEDIA_KIND_GIF: &str = "gif";

/// ADR-0047 — the longest `duration_ms` a `voice` attachment may carry
/// (15 minutes). A longer value is dropped at parse and at compose.
pub const MAX_VOICE_DURATION_MS: u32 = 15 * 60 * 1000;

/// ADR-0047 — the longest `duration_ms` a `video_note` attachment may
/// carry (60 seconds). A longer value is dropped at parse and at compose.
pub const MAX_VIDEO_NOTE_DURATION_MS: u32 = 60 * 1000;

/// ADR-0047 — the largest decoded `waveform` (one amplitude byte per bar).
pub const MAX_WAVEFORM_BYTES: usize = 128;

/// Base64 (standard alphabet, padded) length of [`MAX_WAVEFORM_BYTES`].
/// Checked before decoding so an oversize string is refused without
/// allocating its decode.
const MAX_WAVEFORM_B64_LEN: usize = MAX_WAVEFORM_BYTES.div_ceil(3) * 4;

/// ADR-0047 — the largest `width` / `height` an attachment or its thumb
/// may carry, in pixels.
pub const MAX_MEDIA_DIMENSION: u32 = 4096;

/// ADR-0047 — the largest plaintext `size` of a [`ThumbRef`] (256 KiB).
pub const MAX_THUMB_BYTES: u64 = 256 * 1024;

// ============================================================================
// Encrypted attachment
// ============================================================================

/// Result of [`encrypt_attachment`]: opaque ciphertext + the
/// secrets needed to decrypt it. The caller PUTs `ciphertext` to
/// `/blob/<hex(blob_id)>` and rides the `key` + `nonce` inside an
/// [`AttachmentRef`] in the MLS Application message.
///
/// `Debug` is intentionally redacted on `key` (the inner
/// `AeadKey`'s own `Debug` is already opaque) so accidental log
/// lines don't leak the CEK. The other fields are public ciphertext
/// + nonce + blob_id, fine to print at debug-level.
#[derive(Debug)]
pub struct EncryptedAttachment {
    /// Ciphertext bytes (= ChaCha20-Poly1305 over plaintext, AAD
    /// empty). Upload verbatim; the storage server hashes them on
    /// PUT to verify the `blob_id` in the URL.
    pub ciphertext: Vec<u8>,
    /// Fresh random 32-byte CEK. Travels in the application
    /// message; never sent to the storage server.
    pub key: AeadKey,
    /// Fresh random 12-byte nonce. Travels next to `key`.
    pub nonce: [u8; NONCE_LEN],
    /// `SHA-256(ciphertext)` — the storage service's content
    /// address. The sender's PUT URL is `/blob/<hex(blob_id)>`;
    /// the receiver's GET URL is the same.
    pub blob_id: [u8; BLOB_ID_LEN],
}

/// Encrypt `plaintext` under a fresh random CEK + nonce. Returns
/// the ciphertext, the secrets needed to recover the plaintext,
/// and the content-addressed `blob_id` the sender PUTs to.
///
/// # Errors
///
/// - [`AttachmentError::TooLarge`] if `plaintext.len()` exceeds
///   [`MAX_ATTACHMENT_BYTES`].
/// - [`AttachmentError::Crypto`] on an AEAD failure (effectively
///   impossible for valid inputs — surfaced for API completeness).
pub fn encrypt_attachment(plaintext: &[u8]) -> Result<EncryptedAttachment, AttachmentError> {
    if plaintext.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::TooLarge {
            got: plaintext.len(),
            max: MAX_ATTACHMENT_BYTES,
        });
    }
    let mut key_bytes = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key_bytes);
    let key = AeadKey::new(key_bytes);
    // Hint to the optimiser that the local copy is dead — the
    // canonical bytes live inside `AeadKey` from here on. The
    // `key_bytes` array is moved-by-value into `AeadKey::new`
    // anyway; the call below is paranoia (free in release after
    // dead-store elimination, costs ~32 byte writes in debug).
    // Zeroize the local copy — the canonical bytes live inside
    // `AeadKey` (which zeroizes on drop) from here on.
    key_bytes.zeroize();

    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let ciphertext = seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &[], plaintext)
        .map_err(|_| AttachmentError::Crypto)?;
    let blob_id = sha256(&ciphertext);
    Ok(EncryptedAttachment {
        ciphertext,
        key,
        nonce,
        blob_id,
    })
}

/// Decrypt `ciphertext` under `key` + `nonce`. Returns the plaintext.
///
/// # Errors
///
/// - [`AttachmentError::Crypto`] on AEAD tag mismatch (wrong key /
///   nonce / tampered ciphertext).
pub fn decrypt_attachment(
    ciphertext: &[u8],
    key: &AeadKey,
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, AttachmentError> {
    open(Algorithm::ChaCha20Poly1305, key, nonce, &[], ciphertext)
        .map_err(|_| AttachmentError::Crypto)
}

/// Compute `SHA-256(bytes)` and return the raw 32-byte digest. Public
/// because clients sometimes need to verify a server-returned
/// blob_id matches what they expected (defence in depth: the
/// content-addressed storage layer already guarantees the byte-
/// equality, but a paranoid client can recompute for free).
#[must_use]
pub fn sha256(bytes: &[u8]) -> [u8; BLOB_ID_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut out = [0u8; BLOB_ID_LEN];
    out.copy_from_slice(&hasher.finalize());
    out
}

// ============================================================================
// Wire envelope
// ============================================================================

/// One attachment reference inside a chat-message envelope.
///
/// All byte fields ride the wire base64-encoded with the standard
/// alphabet (with padding) — same encoding the auth + discovery
/// services use. Sender writes; receiver reads + GETs the blob;
/// nothing here ever crosses the storage-service wire (the
/// storage server only sees the `ciphertext` bytes and the URL
/// path's hex blob_id).
///
/// The ADR-0047 media fields (`kind` … `thumb`) are optional and
/// bounded. Deserialization goes through a lenient wire shape: a
/// field of the wrong type or out of its bound is dropped, never
/// failing the ref or the envelope (see the crate docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AttachmentRefWire")]
pub struct AttachmentRef {
    /// Base64 of [`EncryptedAttachment::blob_id`] (32 bytes raw,
    /// 44 chars base64 with padding). The hex form rides in the
    /// `/blob/<hex>` URL path; the wire envelope uses base64 for
    /// uniformity with the other byte fields.
    pub blob_id: String,
    /// Base64 of the 32-byte ChaCha20-Poly1305 CEK.
    pub key: String,
    /// Base64 of the 12-byte AEAD nonce.
    pub nonce: String,
    /// MIME type (e.g. `image/jpeg`, `application/pdf`). Optional —
    /// receivers that don't recognise the value render generic
    /// "attachment" affordances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// **Plaintext** size in bytes. Used by the receiver to size a
    /// progress indicator before the GET completes + to detect a
    /// silently truncated blob.
    pub size: u64,
    /// Human-visible filename. Optional — receivers fall back to
    /// the first 8 hex chars of the blob_id when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// ADR-0047 — what the attachment is: [`MEDIA_KIND_VOICE`],
    /// [`MEDIA_KIND_VIDEO_NOTE`], [`MEDIA_KIND_GIF`], or absent for a
    /// plain file. An unknown value is kept verbatim and classifies as
    /// [`MediaKind::Other`], which renders as a file. Chooses the bubble
    /// only — whether anything plays inline is decided by `mime`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// ADR-0047 — playback length in milliseconds. Kept only for a
    /// `voice` (1..=[`MAX_VOICE_DURATION_MS`]) or `video_note`
    /// (1..=[`MAX_VIDEO_NOTE_DURATION_MS`]) attachment; any other kind,
    /// zero, or an out-of-range value is dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// ADR-0047 — base64 (standard alphabet, padded) of 1..=
    /// [`MAX_WAVEFORM_BYTES`] amplitude bytes, one per bar of the voice
    /// bubble. Undecodable, empty or oversize is dropped. Read the bytes
    /// via [`Self::decode_waveform`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waveform: Option<String>,
    /// ADR-0047 — pixel width, 1..=[`MAX_MEDIA_DIMENSION`]; otherwise
    /// dropped. Independent of `height`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// ADR-0047 — pixel height, 1..=[`MAX_MEDIA_DIMENSION`]; otherwise
    /// dropped. Independent of `width`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// ADR-0047 — a second encrypted blob holding a poster frame (the
    /// video note's still). Dropped whole if its own ref is malformed or
    /// its size is outside 1..=[`MAX_THUMB_BYTES`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<ThumbRef>,
}

/// The receiver-side classification of [`AttachmentRef::kind`]
/// (ADR-0047). Never fails: an unrecognised kind is [`Self::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// [`MEDIA_KIND_VOICE`] — a recorded voice message.
    Voice,
    /// [`MEDIA_KIND_VIDEO_NOTE`] — a round video note.
    VideoNote,
    /// [`MEDIA_KIND_GIF`] — a GIF re-uploaded from the GIF proxy.
    Gif,
    /// Any other value — a kind this build does not know. Renders as a
    /// plain file.
    Other,
}

impl MediaKind {
    /// Classify a wire `kind` string. Exact, case-sensitive match.
    #[must_use]
    pub fn from_wire(kind: &str) -> Self {
        match kind {
            MEDIA_KIND_VOICE => Self::Voice,
            MEDIA_KIND_VIDEO_NOTE => Self::VideoNote,
            MEDIA_KIND_GIF => Self::Gif,
            _ => Self::Other,
        }
    }

    /// The wire string for a known kind, `None` for [`Self::Other`].
    #[must_use]
    pub const fn as_wire(self) -> Option<&'static str> {
        match self {
            Self::Voice => Some(MEDIA_KIND_VOICE),
            Self::VideoNote => Some(MEDIA_KIND_VIDEO_NOTE),
            Self::Gif => Some(MEDIA_KIND_GIF),
            Self::Other => None,
        }
    }
}

/// How a client may render an attachment inline (ADR-0047). Returned by
/// [`inline_media_type`]; `None` there means "file chip with Save".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InlineMediaType {
    /// Render as an image (`image/jpeg`, `image/png`, `image/gif`,
    /// `image/webp`).
    Image,
    /// Play as audio (`audio/mp4`).
    Audio,
    /// Play as video (`video/mp4`).
    Video,
}

/// The inline-playback allowlist (ADR-0047), MIME essence → class.
/// `image/svg+xml` is deliberately absent — it can carry script.
const INLINE_MEDIA_ALLOWLIST: [(&str, InlineMediaType); 6] = [
    ("image/jpeg", InlineMediaType::Image),
    ("image/png", InlineMediaType::Image),
    ("image/gif", InlineMediaType::Image),
    ("image/webp", InlineMediaType::Image),
    ("audio/mp4", InlineMediaType::Audio),
    ("video/mp4", InlineMediaType::Video),
];

/// The inline-playback allowlist (ADR-0047): the only MIME types a
/// client may render or play inline. Decided by MIME alone — never by
/// [`AttachmentRef::kind`], which is sender-chosen like the file name.
///
/// Matching is on the MIME essence: parameters after `;` are ignored,
/// surrounding whitespace is trimmed and the comparison is
/// ASCII-case-insensitive (so `audio/mp4;codecs=mp4a.40.2`, which a
/// browser `MediaRecorder` reports, still plays). `image/svg+xml` is
/// deliberately absent — it can carry script.
#[must_use]
pub fn inline_media_type(mime: &str) -> Option<InlineMediaType> {
    let essence = mime.split_once(';').map_or(mime, |(e, _)| e).trim();
    INLINE_MEDIA_ALLOWLIST
        .iter()
        .find(|(allowed, _)| essence.eq_ignore_ascii_case(allowed))
        .map(|(_, class)| *class)
}

/// ADR-0047 — a poster-frame blob carried on an [`AttachmentRef`]. Same
/// encryption and the same base64 encodings as the ref itself: the thumb
/// is its own blob on `services/storage` under its own CEK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ThumbRefWire")]
pub struct ThumbRef {
    /// Base64 of the thumb blob's 32-byte SHA-256 id.
    pub blob_id: String,
    /// Base64 of the thumb's 32-byte CEK.
    pub key: String,
    /// Base64 of the thumb's 12-byte AEAD nonce.
    pub nonce: String,
    /// MIME type of the thumb (normally an image type; check it with
    /// [`inline_media_type`] before rendering).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Plaintext size in bytes, 1..=[`MAX_THUMB_BYTES`].
    pub size: u64,
    /// Pixel width, 1..=[`MAX_MEDIA_DIMENSION`]; otherwise dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Pixel height, 1..=[`MAX_MEDIA_DIMENSION`]; otherwise dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl ThumbRef {
    /// Build a thumb ref from an [`EncryptedAttachment`] of the poster
    /// image. `plaintext_size` is the unencrypted image size.
    ///
    /// Bounds are not checked here; an out-of-bound thumb is dropped by
    /// [`compose`] and by every receiver.
    #[must_use]
    pub fn from_encrypted(
        enc: &EncryptedAttachment,
        plaintext_size: u64,
        mime: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            blob_id: B64.encode(enc.blob_id),
            key: B64.encode(enc.key.as_bytes()),
            nonce: B64.encode(enc.nonce),
            mime,
            size: plaintext_size,
            width,
            height,
        }
    }

    /// Decode the thumb's `blob_id`.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_blob_id(&self) -> Result<[u8; BLOB_ID_LEN], AttachmentError> {
        decode_blob_id_b64(&self.blob_id)
    }

    /// Decode the thumb's CEK.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_key(&self) -> Result<AeadKey, AttachmentError> {
        decode_key_b64(&self.key)
    }

    /// Decode the thumb's nonce.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_nonce(&self) -> Result<[u8; NONCE_LEN], AttachmentError> {
        decode_nonce_b64(&self.nonce)
    }

    /// Apply the ADR-0047 bounds: `None` when the thumb itself is
    /// unusable (a byte field that does not decode to its length, or a
    /// size outside 1..=[`MAX_THUMB_BYTES`]); otherwise the thumb with
    /// any out-of-range dimension dropped.
    fn sanitized(mut self) -> Option<Self> {
        if !thumb_size_in_bounds(self.size) {
            return None;
        }
        if self.decode_blob_id().is_err()
            || self.decode_key().is_err()
            || self.decode_nonce().is_err()
        {
            return None;
        }
        self.width = self.width.filter(|&w| dimension_in_bounds(w));
        self.height = self.height.filter(|&h| dimension_in_bounds(h));
        Some(self)
    }
}

impl AttachmentRef {
    /// Build a wire ref from an [`EncryptedAttachment`] + the
    /// caller-supplied metadata (mime / name / size).
    ///
    /// `size` should be the **plaintext** size — that's what the
    /// UI surfaces to the user; ciphertext is plaintext + 16-byte
    /// tag (negligible delta but conceptually the wire-friendly
    /// choice is plaintext).
    ///
    /// The ADR-0047 media fields start unset; add them with the
    /// `with_*` builders.
    #[must_use]
    pub fn from_encrypted(
        enc: &EncryptedAttachment,
        plaintext_size: u64,
        mime: Option<String>,
        name: Option<String>,
    ) -> Self {
        Self {
            blob_id: B64.encode(enc.blob_id),
            key: B64.encode(enc.key.as_bytes()),
            nonce: B64.encode(enc.nonce),
            mime,
            size: plaintext_size,
            name,
            kind: None,
            duration_ms: None,
            waveform: None,
            width: None,
            height: None,
            thumb: None,
        }
    }

    /// ADR-0047 — set the media `kind` (e.g. [`MEDIA_KIND_VOICE`]).
    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// ADR-0047 — set `duration_ms`. Kept on the wire only for a
    /// `voice` / `video_note` kind within its bound.
    #[must_use]
    pub const fn with_duration_ms(mut self, duration_ms: u32) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// ADR-0047 — set the waveform from raw amplitude bytes (base64-
    /// encoded here). More than [`MAX_WAVEFORM_BYTES`], or none, is
    /// dropped at compose.
    #[must_use]
    pub fn with_waveform(mut self, amplitudes: &[u8]) -> Self {
        self.waveform = Some(B64.encode(amplitudes));
        self
    }

    /// ADR-0047 — set the pixel dimensions. Each is bounded separately.
    #[must_use]
    pub const fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// ADR-0047 — attach a poster-frame thumb.
    #[must_use]
    pub fn with_thumb(mut self, thumb: ThumbRef) -> Self {
        self.thumb = Some(thumb);
        self
    }

    /// ADR-0047 — the typed media kind, `None` when `kind` is absent (a
    /// plain file).
    #[must_use]
    pub fn media_kind(&self) -> Option<MediaKind> {
        self.kind.as_deref().map(MediaKind::from_wire)
    }

    /// ADR-0047 — the decoded waveform amplitudes, `None` when absent or
    /// out of bounds.
    #[must_use]
    pub fn decode_waveform(&self) -> Option<Vec<u8>> {
        self.waveform.as_deref().and_then(decode_waveform_b64)
    }

    /// ADR-0047 — how this attachment may be rendered inline, decided by
    /// its MIME type through [`inline_media_type`]; `None` means a file
    /// chip. Never consults `kind`.
    #[must_use]
    pub fn inline_media_type(&self) -> Option<InlineMediaType> {
        self.mime.as_deref().and_then(inline_media_type)
    }

    /// ADR-0047 — `true` when [`Self::inline_media_type`] allows inline
    /// rendering or playback.
    #[must_use]
    pub fn is_inline_playable(&self) -> bool {
        self.inline_media_type().is_some()
    }

    /// Apply the ADR-0047 bounds to the media fields, dropping each one
    /// that fails. The ref's own fields are untouched. Runs on every
    /// deserialized ref and in [`compose`].
    fn sanitized(mut self) -> Self {
        self.duration_ms = bound_duration(self.kind.as_deref(), self.duration_ms);
        self.waveform = self.waveform.filter(|w| decode_waveform_b64(w).is_some());
        self.width = self.width.filter(|&w| dimension_in_bounds(w));
        self.height = self.height.filter(|&h| dimension_in_bounds(h));
        self.thumb = self.thumb.and_then(ThumbRef::sanitized);
        self
    }

    /// Decode the base64-encoded `blob_id` into 32 raw bytes.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] if the base64 doesn't decode
    /// or the decoded length is not [`BLOB_ID_LEN`].
    pub fn decode_blob_id(&self) -> Result<[u8; BLOB_ID_LEN], AttachmentError> {
        decode_blob_id_b64(&self.blob_id)
    }

    /// Decode the base64-encoded CEK into a zeroize-on-drop wrapper.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_key(&self) -> Result<AeadKey, AttachmentError> {
        decode_key_b64(&self.key)
    }

    /// Decode the base64-encoded nonce.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_nonce(&self) -> Result<[u8; NONCE_LEN], AttachmentError> {
        decode_nonce_b64(&self.nonce)
    }
}

// ---- ADR-0047 lenient wire shapes ----

/// Deserialize an optional field without ever failing the enclosing
/// object: the value is read as untyped JSON, then converted; a value of
/// the wrong type (or `null`) becomes `None`. Only a structurally broken
/// JSON document can fail here, and that already fails the envelope.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// The wire shape [`AttachmentRef`] deserializes through. The pre-
/// ADR-0047 fields keep their strict types; the media fields are
/// lenient, and [`AttachmentRef::sanitized`] applies their bounds.
#[derive(Deserialize)]
struct AttachmentRefWire {
    blob_id: String,
    key: String,
    nonce: String,
    #[serde(default)]
    mime: Option<String>,
    size: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    duration_ms: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    waveform: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    width: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    height: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    thumb: Option<ThumbRef>,
}

impl From<AttachmentRefWire> for AttachmentRef {
    fn from(w: AttachmentRefWire) -> Self {
        Self {
            blob_id: w.blob_id,
            key: w.key,
            nonce: w.nonce,
            mime: w.mime,
            size: w.size,
            name: w.name,
            kind: w.kind,
            duration_ms: w.duration_ms,
            waveform: w.waveform,
            width: w.width,
            height: w.height,
            thumb: w.thumb,
        }
        .sanitized()
    }
}

/// The wire shape [`ThumbRef`] deserializes through. Dimensions are
/// lenient (dropped alone); a malformed ref or an out-of-bound size
/// rejects the whole thumb, which the enclosing lenient `thumb` field
/// then turns into `None`.
#[derive(Deserialize)]
struct ThumbRefWire {
    blob_id: String,
    key: String,
    nonce: String,
    #[serde(default, deserialize_with = "lenient")]
    mime: Option<String>,
    size: u64,
    #[serde(default, deserialize_with = "lenient")]
    width: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    height: Option<u32>,
}

impl TryFrom<ThumbRefWire> for ThumbRef {
    type Error = AttachmentError;

    fn try_from(w: ThumbRefWire) -> Result<Self, Self::Error> {
        Self {
            blob_id: w.blob_id,
            key: w.key,
            nonce: w.nonce,
            mime: w.mime,
            size: w.size,
            width: w.width,
            height: w.height,
        }
        .sanitized()
        .ok_or_else(|| AttachmentError::WireFormat("thumb out of bounds".to_string()))
    }
}

const fn dimension_in_bounds(px: u32) -> bool {
    px > 0 && px <= MAX_MEDIA_DIMENSION
}

/// A duration is kept only for a voice or video-note kind, non-zero and
/// within that kind's limit.
fn bound_duration(kind: Option<&str>, duration_ms: Option<u32>) -> Option<u32> {
    let max = match kind.map(MediaKind::from_wire) {
        Some(MediaKind::Voice) => MAX_VOICE_DURATION_MS,
        Some(MediaKind::VideoNote) => MAX_VIDEO_NOTE_DURATION_MS,
        Some(MediaKind::Gif | MediaKind::Other) | None => return None,
    };
    duration_ms.filter(|&d| d > 0 && d <= max)
}

const fn thumb_size_in_bounds(size: u64) -> bool {
    size > 0 && size <= MAX_THUMB_BYTES
}

/// ADR-0047 media metadata in decoded form, as a history bundle carries it
/// (raw waveform bytes; the blob references stay with the caller, whose
/// fixed-size arrays are valid by construction).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawMediaMetadata {
    /// Media kind string.
    pub kind: Option<String>,
    /// Playback length in milliseconds.
    pub duration_ms: Option<u32>,
    /// Waveform amplitude bytes.
    pub waveform: Option<Vec<u8>>,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
    /// Poster-frame metadata.
    pub thumb: Option<RawThumbMetadata>,
}

/// The poster-frame part of [`RawMediaMetadata`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawThumbMetadata {
    /// Plaintext size in bytes.
    pub size: u64,
    /// MIME type.
    pub mime: Option<String>,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
}

impl RawMediaMetadata {
    /// Apply exactly the bounds a received envelope gets: each field that
    /// fails is dropped, a thumb outside its size limit is dropped whole.
    /// Used where metadata reaches a client by another route than the chat
    /// envelope — the multi-device history bundle — so both routes restore
    /// the same thing.
    #[must_use]
    pub fn bounded(self) -> Self {
        Self {
            duration_ms: bound_duration(self.kind.as_deref(), self.duration_ms),
            waveform: self
                .waveform
                .filter(|w| !w.is_empty() && w.len() <= MAX_WAVEFORM_BYTES),
            width: self.width.filter(|&w| dimension_in_bounds(w)),
            height: self.height.filter(|&h| dimension_in_bounds(h)),
            thumb: self.thumb.and_then(|t| {
                thumb_size_in_bounds(t.size).then(|| RawThumbMetadata {
                    width: t.width.filter(|&w| dimension_in_bounds(w)),
                    height: t.height.filter(|&h| dimension_in_bounds(h)),
                    ..t
                })
            }),
            kind: self.kind,
        }
    }
}

fn decode_waveform_b64(encoded: &str) -> Option<Vec<u8>> {
    if encoded.is_empty() || encoded.len() > MAX_WAVEFORM_B64_LEN {
        return None;
    }
    B64.decode(encoded)
        .ok()
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_WAVEFORM_BYTES)
}

fn decode_blob_id_b64(encoded: &str) -> Result<[u8; BLOB_ID_LEN], AttachmentError> {
    let bytes = B64
        .decode(encoded)
        .map_err(|e| AttachmentError::WireFormat(format!("blob_id base64: {e}")))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        AttachmentError::WireFormat(format!("blob_id length {} != {BLOB_ID_LEN}", v.len()))
    })
}

fn decode_key_b64(encoded: &str) -> Result<AeadKey, AttachmentError> {
    let bytes = B64
        .decode(encoded)
        .map_err(|e| AttachmentError::WireFormat(format!("key base64: {e}")))?;
    if bytes.len() != KEY_LEN {
        return Err(AttachmentError::WireFormat(format!(
            "key length {} != {KEY_LEN}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; KEY_LEN];
    arr.copy_from_slice(&bytes);
    let key = AeadKey::new(arr);
    arr.zeroize();
    Ok(key)
}

fn decode_nonce_b64(encoded: &str) -> Result<[u8; NONCE_LEN], AttachmentError> {
    let bytes = B64
        .decode(encoded)
        .map_err(|e| AttachmentError::WireFormat(format!("nonce base64: {e}")))?;
    bytes.try_into().map_err(|v: Vec<u8>| {
        AttachmentError::WireFormat(format!("nonce length {} != {NONCE_LEN}", v.len()))
    })
}

/// The JSON envelope an MLS Application message carries. v1 fields
/// listed below; future versions add fields with `#[serde(default)]`
/// so unknown-field forward-compat works without a wire-format
/// version bump.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Wire-format version. Must be [`WIRE_VERSION`] for the parser
    /// to accept the message; otherwise the receiver falls back to
    /// "[unsupported v$N message]" placeholder text.
    pub v: u8,
    /// User-visible text. Empty string is valid (attachment-only
    /// message). The serde `default` lets parsers tolerate
    /// upcoming v1.x variants that introduce text-less messages
    /// (e.g. system-generated event markers).
    #[serde(default)]
    pub text: String,
    /// Zero or more attachment references. Omitted from the
    /// serialised form when empty to keep text-only messages
    /// byte-identical to the simplest possible JSON envelope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    /// ADR-0023 — if set, this message EDITS the message whose DS
    /// `messageId` equals this value; `text` is the new body. At
    /// most one of `edit_of` / `delete_of` / `react_to` is set on a
    /// well-formed message; more than one degrades to a plain chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_of: Option<i64>,
    /// ADR-0023 — if set, this message DELETES (tombstones) the
    /// message whose DS `messageId` equals this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_of: Option<i64>,
    /// ADR-0023 — if set, this message REACTS to the message whose
    /// DS `messageId` equals this value with [`Self::emoji`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub react_to: Option<i64>,
    /// ADR-0023 — the reaction emoji, paired with `react_to`. Capped
    /// at [`MAX_EMOJI_BYTES`]; an absent/empty/over-long value with a
    /// set `react_to` degrades the message to a plain chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    /// ADR-0024 — if set, this is an ephemeral TYPING signal
    /// (`true` = started, `false` = stopped). Never persisted by the
    /// receiver; auto-expires after a few seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typing: Option<bool>,
    /// ADR-0024 — if set, this is a READ RECEIPT: the sender has read
    /// up to (and including) the message whose DS `messageId` equals
    /// this value. Monotone per sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_up_to: Option<i64>,
}

impl Envelope {
    /// A blank v1 envelope — all op fields `None`, used as the base
    /// for the `compose_*` builders via struct-update syntax so a new
    /// optional field does not have to be spelled out at every site.
    #[must_use]
    const fn base() -> Self {
        Self {
            v: WIRE_VERSION,
            text: String::new(),
            attachments: Vec::new(),
            edit_of: None,
            delete_of: None,
            react_to: None,
            emoji: None,
            typing: None,
            read_up_to: None,
        }
    }
}

/// The receiver-side classification of a decoded [`Envelope`]
/// (ADR-0023). A plain message is [`Self::Chat`]; the three op
/// variants carry the DS `messageId` of the message they act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageBody {
    /// A normal text + attachments message.
    Chat {
        /// User-visible text.
        text: String,
        /// Attachment references.
        attachments: Vec<AttachmentRef>,
    },
    /// Replace the target message's body with `text`.
    Edit {
        /// DS `messageId` of the edited message.
        target: i64,
        /// The new body.
        text: String,
    },
    /// Tombstone the target message.
    Delete {
        /// DS `messageId` of the deleted message.
        target: i64,
    },
    /// React to the target message with `emoji`.
    Reaction {
        /// DS `messageId` of the reacted-to message.
        target: i64,
        /// The reaction emoji (validated, length-capped).
        emoji: String,
    },
    /// ADR-0024 — an ephemeral typing signal.
    Typing {
        /// `true` = started typing, `false` = stopped.
        is_typing: bool,
    },
    /// ADR-0024 — a read receipt: the sender has read up to this
    /// DS `messageId` (inclusive), monotone per sender.
    ReadReceipt {
        /// The highest `messageId` the sender has read.
        up_to: i64,
    },
}

impl Envelope {
    /// Classify a parsed v1 envelope into a [`MessageBody`]. Exactly
    /// one op field may be set on a well-formed message; an ambiguous
    /// (more than one set) or malformed (reaction without a valid
    /// emoji) envelope falls back to [`MessageBody::Chat`] — never
    /// trust a half-formed op to mutate another message (fail-safe,
    /// the same posture the rest of the parser takes).
    #[must_use]
    fn into_body(self) -> MessageBody {
        let op_count = u8::from(self.edit_of.is_some())
            + u8::from(self.delete_of.is_some())
            + u8::from(self.react_to.is_some())
            + u8::from(self.typing.is_some())
            + u8::from(self.read_up_to.is_some());
        if op_count == 1 {
            if let Some(target) = self.delete_of {
                return MessageBody::Delete { target };
            }
            if let Some(target) = self.edit_of {
                return MessageBody::Edit {
                    target,
                    text: self.text,
                };
            }
            if let Some(is_typing) = self.typing {
                return MessageBody::Typing { is_typing };
            }
            if let Some(up_to) = self.read_up_to {
                return MessageBody::ReadReceipt { up_to };
            }
            if let Some(target) = self.react_to {
                if let Some(emoji) = self.emoji {
                    if !emoji.is_empty() && emoji.len() <= MAX_EMOJI_BYTES {
                        return MessageBody::Reaction { target, emoji };
                    }
                }
                // react_to set without a valid emoji → degrade.
                return MessageBody::Chat {
                    text: self.text,
                    attachments: self.attachments,
                };
            }
        }
        MessageBody::Chat {
            text: self.text,
            attachments: self.attachments,
        }
    }
}

/// Encode `(text, attachments)` as the canonical JSON envelope. The
/// returned bytes are what the caller hands to
/// `Group.createApplicationMessage(identity, bytes)`.
///
/// The envelope is JSON for cross-platform parity (every client
/// already has a JSON codec on hand). Binary framing would save a
/// few bytes per message but at the cost of duplicate
/// serialisation logic in three platform-side languages — the
/// readability win matters more in Phase 1 alpha.
///
/// ADR-0047 media metadata on each ref passes through the same bounds a
/// receiver applies: a field that fails its bound is dropped here, so the
/// wire never carries a value every receiver would discard and the
/// sender's `compose → parse` view matches everyone else's.
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if `serde_json::to_vec` fails
/// (essentially impossible for valid inputs).
pub fn compose(text: &str, attachments: Vec<AttachmentRef>) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        text: text.to_string(),
        attachments: attachments
            .into_iter()
            .map(AttachmentRef::sanitized)
            .collect(),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope).map_err(|e| AttachmentError::WireFormat(format!("compose: {e}")))
}

/// Encode an EDIT of the message whose DS `messageId` is `target`,
/// replacing its body with `new_text` (ADR-0023). The receiver
/// applies it only if the editor is the target's original sender
/// (the author-only gate lives in the client; this just frames the
/// wire bytes).
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if serialisation fails.
pub fn compose_edit(target: i64, new_text: &str) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        text: new_text.to_string(),
        edit_of: Some(target),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope)
        .map_err(|e| AttachmentError::WireFormat(format!("compose_edit: {e}")))
}

/// Encode a DELETE (tombstone) of the message whose DS `messageId`
/// is `target` (ADR-0023).
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if serialisation fails.
pub fn compose_delete(target: i64) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        delete_of: Some(target),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope)
        .map_err(|e| AttachmentError::WireFormat(format!("compose_delete: {e}")))
}

/// Encode a REACTION (`emoji`) to the message whose DS `messageId`
/// is `target` (ADR-0023). The emoji is length-capped at
/// [`MAX_EMOJI_BYTES`]; an over-long value is refused here rather
/// than silently degrading on the receiver.
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if `emoji` is empty or exceeds
/// [`MAX_EMOJI_BYTES`], or if serialisation fails.
pub fn compose_reaction(target: i64, emoji: &str) -> Result<Vec<u8>, AttachmentError> {
    if emoji.is_empty() || emoji.len() > MAX_EMOJI_BYTES {
        return Err(AttachmentError::WireFormat(format!(
            "compose_reaction: emoji length {} not in 1..={MAX_EMOJI_BYTES}",
            emoji.len()
        )));
    }
    let envelope = Envelope {
        react_to: Some(target),
        emoji: Some(emoji.to_string()),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope)
        .map_err(|e| AttachmentError::WireFormat(format!("compose_reaction: {e}")))
}

/// Encode an ephemeral TYPING signal (ADR-0024): `true` = started,
/// `false` = stopped. The receiver shows it transiently and never
/// persists it; the sender throttles + auto-stops it client-side.
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if serialisation fails.
pub fn compose_typing(is_typing: bool) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        typing: Some(is_typing),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope)
        .map_err(|e| AttachmentError::WireFormat(format!("compose_typing: {e}")))
}

/// Encode a READ RECEIPT (ADR-0024): the sender has read up to (and
/// including) the message whose DS `messageId` is `up_to`. The
/// receiver tracks it as a monotone per-sender watermark.
///
/// # Errors
///
/// [`AttachmentError::WireFormat`] if serialisation fails.
pub fn compose_read_receipt(up_to: i64) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        read_up_to: Some(up_to),
        ..Envelope::base()
    };
    serde_json::to_vec(&envelope)
        .map_err(|e| AttachmentError::WireFormat(format!("compose_read_receipt: {e}")))
}

/// Decode an MLS Application message body. Returns a tuple of the
/// extracted `(text, attachments)`.
///
/// **Legacy fallback**: if `bytes` is not valid JSON, treat it as
/// bare UTF-8 text with no attachments. This keeps the receiver
/// compatible with pre-1.19 chat messages (which are raw UTF-8) +
/// with simple test harnesses that don't bother to wrap.
///
/// If the JSON parses but the `v` field is not [`WIRE_VERSION`],
/// returns the placeholder `("[unsupported v$N message]", [])`.
/// Future versions land via new `WIRE_VERSION` values; v1 clients
/// surface the placeholder rather than mis-rendering.
#[must_use]
pub fn parse(bytes: &[u8]) -> (String, Vec<AttachmentRef>) {
    match serde_json::from_slice::<Envelope>(bytes) {
        Ok(env) if env.v == WIRE_VERSION => (env.text, env.attachments),
        Ok(env) => (format!("[unsupported v{} message]", env.v), Vec::new()),
        Err(_) => (
            // Bare-UTF-8 fallback. Lossy on malformed bytes —
            // surfaces as `\u{FFFD}` replacement chars — but
            // matches the "graceful fallback" policy the rest of
            // the chat receive path already takes (cf.
            // `ChatClient.pollMessages` UTF-8 lossy decode).
            String::from_utf8_lossy(bytes).into_owned(),
            Vec::new(),
        ),
    }
}

/// Decode an MLS Application message body into a classified
/// [`MessageBody`] (ADR-0023). This is the richer sibling of
/// [`parse`]: it surfaces whether the message is a plain chat or an
/// edit / delete / reaction op so the receiver can route it.
///
/// Fail-safe like [`parse`]: a non-JSON body becomes a bare-UTF-8
/// [`MessageBody::Chat`]; a wrong-`v` envelope becomes a placeholder
/// chat; an ambiguous or malformed op (more than one op field, or a
/// reaction without a valid emoji) degrades to chat. The caller's
/// author-only gate (ADR-0023) decides whether to apply an op — this
/// function only frames it.
#[must_use]
pub fn parse_body(bytes: &[u8]) -> MessageBody {
    match serde_json::from_slice::<Envelope>(bytes) {
        Ok(env) if env.v == WIRE_VERSION => env.into_body(),
        Ok(env) => MessageBody::Chat {
            text: format!("[unsupported v{} message]", env.v),
            attachments: Vec::new(),
        },
        Err(_) => MessageBody::Chat {
            text: String::from_utf8_lossy(bytes).into_owned(),
            attachments: Vec::new(),
        },
    }
}

// ============================================================================
// Errors
// ============================================================================

/// Error vocabulary for attachment operations. Boundary-friendly —
/// every variant carries enough context for the caller to surface a
/// useful message to the UI.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    /// Plaintext exceeds [`MAX_ATTACHMENT_BYTES`].
    #[error("attachment too large: got {got} bytes, max {max}")]
    TooLarge {
        /// Length the caller passed.
        got: usize,
        /// Hard ceiling (`MAX_ATTACHMENT_BYTES`).
        max: usize,
    },
    /// Generic AEAD failure — seal/open returned an error. Almost
    /// always means a wrong key / nonce / tampered ciphertext for
    /// the open path; for seal it's a library bug.
    #[error("attachment AEAD failure")]
    Crypto,
    /// Wire-format issue — bad base64, wrong-length byte field, JSON
    /// parse failure on a buffer we expected to be JSON. The inner
    /// message is for logs only.
    #[error("attachment wire format: {0}")]
    WireFormat(String),
}

/// Fuzz-only entry point. Marked `#[doc(hidden)]` — the sole harness
/// surface for `crates/lakko-attachments/fuzz/`, **not** part of the
/// supported public API.
#[doc(hidden)]
pub mod fuzz_api {
    use super::{
        compose, parse, parse_body, AttachmentRef, MediaKind, MessageBody, MAX_MEDIA_DIMENSION,
        MAX_THUMB_BYTES, MAX_VIDEO_NOTE_DURATION_MS, MAX_VOICE_DURATION_MS, MAX_WAVEFORM_BYTES,
    };

    /// ADR-0047 property oracle over attacker-controlled bytes.
    ///
    /// Every media field that survives [`parse`] is within its bound,
    /// and sanitization is idempotent (re-composing the parsed refs and
    /// parsing again yields the same refs). The harness asserts the
    /// return, so a bound that stops being enforced — or a sanitizer
    /// that changes its answer on its own output — becomes a crash.
    #[must_use]
    pub fn media_metadata_within_bounds(bytes: &[u8]) -> bool {
        let (text, refs) = parse(bytes);
        if !refs.iter().all(ref_within_bounds) {
            return false;
        }
        let Ok(recomposed) = compose(&text, refs.clone()) else {
            return false;
        };
        parse(&recomposed).1 == refs
    }

    fn ref_within_bounds(r: &AttachmentRef) -> bool {
        const fn dim_ok(px: Option<u32>) -> bool {
            match px {
                None => true,
                Some(p) => p > 0 && p <= MAX_MEDIA_DIMENSION,
            }
        }
        let duration_ok = match (r.duration_ms, r.media_kind()) {
            (None, _) => true,
            (Some(d), Some(MediaKind::Voice)) => d > 0 && d <= MAX_VOICE_DURATION_MS,
            (Some(d), Some(MediaKind::VideoNote)) => d > 0 && d <= MAX_VIDEO_NOTE_DURATION_MS,
            (Some(_), _) => false,
        };
        let waveform_ok = r.waveform.is_none()
            || r.decode_waveform()
                .is_some_and(|w| !w.is_empty() && w.len() <= MAX_WAVEFORM_BYTES);
        let thumb_ok = r.thumb.as_ref().map_or(true, |t| {
            t.size > 0
                && t.size <= MAX_THUMB_BYTES
                && t.decode_blob_id().is_ok()
                && t.decode_key().is_ok()
                && t.decode_nonce().is_ok()
                && dim_ok(t.width)
                && dim_ok(t.height)
        });
        duration_ok && waveform_ok && dim_ok(r.width) && dim_ok(r.height) && thumb_ok
    }

    /// Drive the receiver-side message-body decode chain on
    /// attacker-controlled bytes.
    ///
    /// The input is an MLS Application message body, which a malicious
    /// or compromised group member can craft arbitrarily. The harness
    /// runs the full receive path a peer's message reaches: [`parse`]
    /// (the `serde_json::from_slice::<Envelope>` decode → the
    /// schema-version gate → the bare-UTF-8 `from_utf8_lossy` fallback),
    /// then the per-ref base64 decoders ([`super::AttachmentRef::decode_blob_id`]
    /// / `decode_key` / `decode_nonce`, each a base64 decode + a fixed
    /// length check) on every decoded reference. Any panic / overflow /
    /// slice-out-of-bounds here is a vulnerability.
    #[must_use]
    pub fn parse_message_body(bytes: &[u8]) -> bool {
        let (_text, refs) = parse(bytes);
        for r in &refs {
            let _ = r.decode_blob_id();
            let _ = r.decode_key();
            let _ = r.decode_nonce();
            // ADR-0047: the media-metadata readers a client calls on
            // every ref, including the nested thumb's own decoders.
            let _ = (r.media_kind(), r.decode_waveform(), r.inline_media_type());
            if let Some(t) = &r.thumb {
                let _ = (t.decode_blob_id(), t.decode_key(), t.decode_nonce());
                let _ = t.mime.as_deref().map(super::inline_media_type);
            }
        }
        // ADR-0023: also drive the richer classifier on the same
        // attacker-controlled bytes — the edit/delete/reaction op
        // fields + the emoji length cap + the >1-op-field degrade
        // path all sit on this surface. Touch every variant's fields
        // so the fuzzer explores them.
        match parse_body(bytes) {
            MessageBody::Chat { text, attachments } => {
                let _ = (text.len(), attachments.len());
            },
            MessageBody::Edit { target, text } => {
                let _ = (target, text.len());
            },
            MessageBody::Delete { target } => {
                let _ = target;
            },
            MessageBody::Reaction { target, emoji } => {
                let _ = (target, emoji.len());
            },
            MessageBody::Typing { is_typing } => {
                let _ = is_typing;
            },
            MessageBody::ReadReceipt { up_to } => {
                let _ = up_to;
            },
        }
        !refs.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encrypts_and_decrypts() {
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        let enc = encrypt_attachment(plaintext).unwrap();
        assert_eq!(enc.blob_id, sha256(&enc.ciphertext));
        let decrypted = decrypt_attachment(&enc.ciphertext, &enc.key, &enc.nonce).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn two_encrypts_of_same_plaintext_yield_different_blob_ids() {
        // Fresh random CEK + nonce per call → different ciphertext
        // → different SHA-256. Same plaintext, different blob_id.
        let plaintext = b"identical bytes";
        let a = encrypt_attachment(plaintext).unwrap();
        let b = encrypt_attachment(plaintext).unwrap();
        assert_ne!(a.blob_id, b.blob_id);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let enc = encrypt_attachment(b"data").unwrap();
        let mut tampered = enc.ciphertext.clone();
        tampered[0] ^= 0x01;
        let err = decrypt_attachment(&tampered, &enc.key, &enc.nonce).unwrap_err();
        assert!(matches!(err, AttachmentError::Crypto));
    }

    #[test]
    fn encrypt_rejects_oversize_plaintext() {
        // Vec::new() with capacity max+1 — don't actually allocate,
        // just lie about the length via a const-size sample we
        // never pass through AEAD.
        let oversize = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        let err = encrypt_attachment(&oversize).unwrap_err();
        assert!(matches!(err, AttachmentError::TooLarge { .. }));
    }

    #[test]
    fn ref_round_trips_through_encode_decode() {
        let enc = encrypt_attachment(b"hello world").unwrap();
        let r = AttachmentRef::from_encrypted(
            &enc,
            11,
            Some("text/plain".to_string()),
            Some("hello.txt".to_string()),
        );
        assert_eq!(r.decode_blob_id().unwrap(), enc.blob_id);
        assert_eq!(r.decode_nonce().unwrap(), enc.nonce);
        // Round-trip the key through wire encoding: encrypt under
        // it then decrypt under the round-tripped copy.
        let recovered = r.decode_key().unwrap();
        let pt = decrypt_attachment(&enc.ciphertext, &recovered, &enc.nonce).unwrap();
        assert_eq!(pt, b"hello world");
    }

    #[test]
    fn envelope_compose_parse_round_trip_text_only() {
        let bytes = compose("hi there", Vec::new()).unwrap();
        let (text, refs) = parse(&bytes);
        assert_eq!(text, "hi there");
        assert!(refs.is_empty());
    }

    #[test]
    fn envelope_compose_omits_empty_attachments() {
        let bytes = compose("hi", Vec::new()).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        // `Vec::is_empty` skip-serializing means the wire shape is
        // `{"v":1,"text":"hi"}` rather than `…,"attachments":[]`.
        assert!(!s.contains("attachments"), "got {s}");
    }

    #[test]
    fn envelope_compose_parse_round_trip_with_attachments() {
        let enc = encrypt_attachment(b"image bytes").unwrap();
        let r = AttachmentRef::from_encrypted(
            &enc,
            11,
            Some("image/png".to_string()),
            Some("cat.png".to_string()),
        );
        let bytes = compose("look at this", vec![r.clone()]).unwrap();
        let (text, refs) = parse(&bytes);
        assert_eq!(text, "look at this");
        assert_eq!(refs, vec![r]);
    }

    #[test]
    fn parse_falls_back_to_bare_utf8_on_non_json() {
        let (text, refs) = parse(b"plain old text");
        assert_eq!(text, "plain old text");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_surfaces_placeholder_on_unknown_version() {
        // Hand-roll a v99 envelope so we don't accidentally bump
        // `WIRE_VERSION` later and break the test.
        let evil = br#"{"v":99,"text":"future","attachments":[]}"#;
        let (text, refs) = parse(evil);
        assert_eq!(text, "[unsupported v99 message]");
        assert!(refs.is_empty());
    }

    #[test]
    fn parse_tolerates_unknown_fields() {
        // Forward-compat for v1.x additions like `reply_to`.
        let json = br#"{"v":1,"text":"hi","attachments":[],"reply_to":"abc"}"#;
        let (text, refs) = parse(json);
        assert_eq!(text, "hi");
        assert!(refs.is_empty());
    }

    // ---- ADR-0023: edit / delete / reaction ----

    #[test]
    fn edit_round_trips_through_compose_parse_body() {
        let bytes = compose_edit(42, "fixed typo").unwrap();
        match parse_body(&bytes) {
            MessageBody::Edit { target, text } => {
                assert_eq!(target, 42);
                assert_eq!(text, "fixed typo");
            },
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    #[test]
    fn delete_round_trips_through_compose_parse_body() {
        let bytes = compose_delete(7).unwrap();
        assert_eq!(parse_body(&bytes), MessageBody::Delete { target: 7 });
    }

    #[test]
    fn reaction_round_trips_through_compose_parse_body() {
        let bytes = compose_reaction(99, "\u{1F44D}").unwrap();
        match parse_body(&bytes) {
            MessageBody::Reaction { target, emoji } => {
                assert_eq!(target, 99);
                assert_eq!(emoji, "\u{1F44D}");
            },
            other => panic!("expected Reaction, got {other:?}"),
        }
    }

    #[test]
    fn plain_chat_parses_as_chat_body() {
        let bytes = compose("hello", Vec::new()).unwrap();
        assert_eq!(
            parse_body(&bytes),
            MessageBody::Chat {
                text: "hello".to_string(),
                attachments: Vec::new(),
            }
        );
    }

    #[test]
    fn op_envelope_back_compat_parse_reads_text() {
        // The legacy `parse` (text, attachments) still works on an op
        // envelope — it just surfaces the text, ignoring the op. This
        // is the forward-compat degrade for any caller not yet routing
        // via parse_body.
        let bytes = compose_edit(1, "new body").unwrap();
        let (text, refs) = parse(&bytes);
        assert_eq!(text, "new body");
        assert!(refs.is_empty());
    }

    #[test]
    fn ambiguous_two_op_fields_degrade_to_chat() {
        // A hand-rolled envelope with BOTH edit_of and delete_of must
        // NOT be trusted as either op — fail-safe to chat.
        let json = br#"{"v":1,"text":"x","edit_of":1,"delete_of":2}"#;
        match parse_body(json) {
            MessageBody::Chat { text, .. } => assert_eq!(text, "x"),
            other => panic!("expected Chat degrade, got {other:?}"),
        }
    }

    #[test]
    fn reaction_without_emoji_degrades_to_chat() {
        let json = br#"{"v":1,"react_to":5}"#;
        assert!(matches!(parse_body(json), MessageBody::Chat { .. }));
    }

    #[test]
    fn reaction_with_overlong_emoji_degrades_to_chat() {
        // A react_to with an emoji past the cap degrades rather than
        // carrying an unbounded string into the reaction path.
        let big = "x".repeat(MAX_EMOJI_BYTES + 1);
        let json = format!(r#"{{"v":1,"react_to":5,"emoji":"{big}"}}"#);
        assert!(matches!(
            parse_body(json.as_bytes()),
            MessageBody::Chat { .. }
        ));
    }

    #[test]
    fn compose_reaction_rejects_empty_and_overlong_emoji() {
        assert!(compose_reaction(1, "").is_err());
        let big = "x".repeat(MAX_EMOJI_BYTES + 1);
        assert!(compose_reaction(1, &big).is_err());
    }

    #[test]
    fn unknown_version_body_is_chat_placeholder() {
        let evil = br#"{"v":99,"edit_of":1,"text":"future"}"#;
        match parse_body(evil) {
            MessageBody::Chat { text, .. } => assert_eq!(text, "[unsupported v99 message]"),
            other => panic!("expected placeholder chat, got {other:?}"),
        }
    }

    // ---- ADR-0024: typing / read receipts ----

    #[test]
    fn typing_true_round_trips_through_parse_body() {
        let bytes = compose_typing(true).unwrap();
        assert_eq!(parse_body(&bytes), MessageBody::Typing { is_typing: true });
    }

    #[test]
    fn typing_false_round_trips_through_parse_body() {
        // `false` must classify as a Typing op, not degrade — clearing
        // a typing state is a real signal.
        let bytes = compose_typing(false).unwrap();
        assert_eq!(parse_body(&bytes), MessageBody::Typing { is_typing: false });
    }

    #[test]
    fn read_receipt_round_trips_through_parse_body() {
        let bytes = compose_read_receipt(1042).unwrap();
        assert_eq!(parse_body(&bytes), MessageBody::ReadReceipt { up_to: 1042 });
    }

    #[test]
    fn typing_envelope_is_not_a_chat_bubble() {
        // The legacy `parse` surfaces empty text for a typing envelope
        // (no user-visible content) — a v1.0 client renders nothing.
        let bytes = compose_typing(true).unwrap();
        let (text, refs) = parse(&bytes);
        assert!(text.is_empty());
        assert!(refs.is_empty());
    }

    #[test]
    fn typing_plus_another_op_degrades_to_chat() {
        // Ambiguity (typing alongside edit) must not be trusted as
        // either op — fail-safe to chat.
        let json = br#"{"v":1,"text":"x","typing":true,"edit_of":1}"#;
        assert!(matches!(parse_body(json), MessageBody::Chat { .. }));
    }

    // ---- ADR-0047: media metadata ----

    fn plain_ref(mime: &str, name: &str) -> AttachmentRef {
        let enc = encrypt_attachment(b"media bytes").unwrap();
        AttachmentRef::from_encrypted(&enc, 11, Some(mime.to_string()), Some(name.to_string()))
    }

    fn valid_thumb() -> ThumbRef {
        let enc = encrypt_attachment(b"poster").unwrap();
        ThumbRef::from_encrypted(
            &enc,
            6,
            Some("image/jpeg".to_string()),
            Some(240),
            Some(240),
        )
    }

    /// Build a one-attachment envelope from `r` as JSON, then overwrite
    /// (or add) the given attachment fields with raw JSON values — the
    /// way a hostile or buggy sender would.
    fn envelope_with_ref_fields(
        r: &AttachmentRef,
        fields: &[(&str, serde_json::Value)],
    ) -> Vec<u8> {
        let mut value = serde_json::to_value(r).unwrap();
        let obj = value.as_object_mut().unwrap();
        for (k, v) in fields {
            obj.insert((*k).to_string(), v.clone());
        }
        serde_json::to_vec(&serde_json::json!({"v": 1, "text": "caption", "attachments": [value]}))
            .unwrap()
    }

    fn parse_one(bytes: &[u8]) -> AttachmentRef {
        let (text, refs) = parse(bytes);
        assert_eq!(text, "caption", "envelope must not fall back to raw text");
        assert_eq!(refs.len(), 1, "the attachment must survive");
        refs.into_iter().next().unwrap()
    }

    #[test]
    fn media_fields_round_trip_through_compose_parse() {
        let voice = plain_ref("audio/mp4", "voice-1.m4a")
            .with_kind(MEDIA_KIND_VOICE)
            .with_duration_ms(12_345)
            .with_waveform(&[0, 10, 200, 255])
            .with_dimensions(320, 240)
            .with_thumb(valid_thumb());
        let bytes = compose("caption", vec![voice.clone()]).unwrap();
        let got = parse_one(&bytes);
        assert_eq!(got, voice);
        assert_eq!(got.media_kind(), Some(MediaKind::Voice));
        assert_eq!(got.decode_waveform(), Some(vec![0, 10, 200, 255]));
        assert_eq!(got.thumb.as_ref().unwrap().size, 6);
        // The same survives the classifier path every client uses.
        match parse_body(&bytes) {
            MessageBody::Chat { attachments, .. } => assert_eq!(attachments, vec![voice]),
            other => panic!("expected Chat, got {other:?}"),
        }
    }

    #[test]
    fn old_style_ref_without_media_fields_parses() {
        let enc = encrypt_attachment(b"x").unwrap();
        let json = format!(
            r#"{{"v":1,"text":"caption","attachments":[{{"blob_id":"{}","key":"{}","nonce":"{}","mime":"image/png","size":1,"name":"a.png"}}]}}"#,
            B64.encode(enc.blob_id),
            B64.encode(enc.key.as_bytes()),
            B64.encode(enc.nonce),
        );
        let got = parse_one(json.as_bytes());
        assert_eq!(got.mime.as_deref(), Some("image/png"));
        assert_eq!(got.media_kind(), None);
        assert!(got.duration_ms.is_none() && got.waveform.is_none());
        assert!(got.width.is_none() && got.height.is_none() && got.thumb.is_none());
        // And a ref with no media fields serialises without any of them.
        let s = String::from_utf8(compose("c", vec![got]).unwrap()).unwrap();
        for field in [
            "kind",
            "duration_ms",
            "waveform",
            "width",
            "height",
            "thumb",
        ] {
            assert!(!s.contains(field), "{field} leaked into {s}");
        }
    }

    #[test]
    fn voice_duration_over_bound_drops_only_duration() {
        let r = plain_ref("audio/mp4", "v.m4a")
            .with_kind(MEDIA_KIND_VOICE)
            .with_waveform(&[1, 2, 3]);
        let at_max = parse_one(&envelope_with_ref_fields(
            &r,
            &[("duration_ms", MAX_VOICE_DURATION_MS.into())],
        ));
        assert_eq!(at_max.duration_ms, Some(MAX_VOICE_DURATION_MS));
        let over = parse_one(&envelope_with_ref_fields(
            &r,
            &[("duration_ms", (MAX_VOICE_DURATION_MS + 1).into())],
        ));
        assert_eq!(over.duration_ms, None);
        assert_eq!(over.kind.as_deref(), Some(MEDIA_KIND_VOICE));
        assert_eq!(over.decode_waveform(), Some(vec![1, 2, 3]));
    }

    #[test]
    fn video_note_duration_over_bound_drops_only_duration() {
        let r = plain_ref("video/mp4", "n.mp4")
            .with_kind(MEDIA_KIND_VIDEO_NOTE)
            .with_dimensions(480, 480);
        let at_max = parse_one(&envelope_with_ref_fields(
            &r,
            &[("duration_ms", MAX_VIDEO_NOTE_DURATION_MS.into())],
        ));
        assert_eq!(at_max.duration_ms, Some(MAX_VIDEO_NOTE_DURATION_MS));
        // A voice-legal duration is still too long for a video note.
        let over = parse_one(&envelope_with_ref_fields(
            &r,
            &[("duration_ms", (MAX_VIDEO_NOTE_DURATION_MS + 1).into())],
        ));
        assert_eq!(over.duration_ms, None);
        assert_eq!((over.width, over.height), (Some(480), Some(480)));
    }

    #[test]
    fn duration_is_dropped_for_other_kinds_and_zero() {
        for kind in [Some(MEDIA_KIND_GIF), Some("sticker"), None] {
            let mut r = plain_ref("image/gif", "g.gif");
            r.kind = kind.map(str::to_string);
            let got = parse_one(&envelope_with_ref_fields(
                &r,
                &[("duration_ms", 1000.into())],
            ));
            assert_eq!(got.duration_ms, None, "kind {kind:?}");
            assert_eq!(got.kind.as_deref(), kind);
        }
        let voice = plain_ref("audio/mp4", "v.m4a").with_kind(MEDIA_KIND_VOICE);
        let zero = parse_one(&envelope_with_ref_fields(
            &voice,
            &[("duration_ms", 0.into())],
        ));
        assert_eq!(zero.duration_ms, None);
    }

    #[test]
    fn waveform_out_of_bound_drops_only_waveform() {
        let r = plain_ref("audio/mp4", "v.m4a")
            .with_kind(MEDIA_KIND_VOICE)
            .with_duration_ms(5000);
        let max = vec![7u8; MAX_WAVEFORM_BYTES];
        let kept = parse_one(&envelope_with_ref_fields(
            &r,
            &[("waveform", B64.encode(&max).into())],
        ));
        assert_eq!(kept.decode_waveform(), Some(max));
        for bad in [
            B64.encode(vec![7u8; MAX_WAVEFORM_BYTES + 1]),
            String::new(),
            "not base64!".to_string(),
        ] {
            let got = parse_one(&envelope_with_ref_fields(
                &r,
                &[("waveform", bad.clone().into())],
            ));
            assert_eq!(got.waveform, None, "waveform {bad:?}");
            assert_eq!(got.duration_ms, Some(5000));
        }
    }

    #[test]
    fn dimensions_out_of_bound_drop_individually() {
        let r = plain_ref("image/gif", "g.gif").with_kind(MEDIA_KIND_GIF);
        let got = parse_one(&envelope_with_ref_fields(
            &r,
            &[
                ("width", (MAX_MEDIA_DIMENSION + 1).into()),
                ("height", MAX_MEDIA_DIMENSION.into()),
            ],
        ));
        assert_eq!((got.width, got.height), (None, Some(MAX_MEDIA_DIMENSION)));
        let zero = parse_one(&envelope_with_ref_fields(
            &r,
            &[("width", 5.into()), ("height", 0.into())],
        ));
        assert_eq!((zero.width, zero.height), (Some(5), None));
        assert_eq!(zero.kind.as_deref(), Some(MEDIA_KIND_GIF));
    }

    #[test]
    fn thumb_out_of_bound_or_malformed_drops_only_thumb() {
        let r = plain_ref("video/mp4", "n.mp4")
            .with_kind(MEDIA_KIND_VIDEO_NOTE)
            .with_duration_ms(3000);
        let mut at_max = valid_thumb();
        at_max.size = MAX_THUMB_BYTES;
        let kept = parse_one(&envelope_with_ref_fields(
            &r,
            &[("thumb", serde_json::to_value(&at_max).unwrap())],
        ));
        assert_eq!(kept.thumb, Some(at_max));

        let mut oversize = valid_thumb();
        oversize.size = MAX_THUMB_BYTES + 1;
        let mut zero = valid_thumb();
        zero.size = 0;
        let mut bad_key = valid_thumb();
        bad_key.key = B64.encode([0u8; 5]);
        let mut bad_blob = valid_thumb();
        bad_blob.blob_id = "%%%".to_string();
        for bad in [oversize, zero, bad_key, bad_blob] {
            let got = parse_one(&envelope_with_ref_fields(
                &r,
                &[("thumb", serde_json::to_value(&bad).unwrap())],
            ));
            assert_eq!(got.thumb, None, "thumb {bad:?}");
            assert_eq!(got.duration_ms, Some(3000));
        }

        // An out-of-range thumb dimension drops that dimension, not the thumb.
        let mut wide = serde_json::to_value(valid_thumb()).unwrap();
        wide["width"] = (MAX_MEDIA_DIMENSION + 1).into();
        let got = parse_one(&envelope_with_ref_fields(&r, &[("thumb", wide)]));
        let thumb = got.thumb.unwrap();
        assert_eq!((thumb.width, thumb.height), (None, Some(240)));
    }

    #[test]
    fn wrongly_typed_media_fields_never_fail_the_envelope() {
        let r = plain_ref("audio/mp4", "v.m4a")
            .with_kind(MEDIA_KIND_VOICE)
            .with_duration_ms(4000);
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("duration_ms", "abc".into()),
            ("duration_ms", (-1).into()),
            ("duration_ms", 1.5.into()),
            ("duration_ms", u64::MAX.into()),
            ("duration_ms", serde_json::Value::Null),
            ("thumb", 5.into()),
            ("thumb", serde_json::json!({"blob_id": 5})),
            ("thumb", serde_json::json!([])),
            ("waveform", 7.into()),
            ("waveform", serde_json::json!({"a": [1, 2]})),
            ("width", "wide".into()),
            ("height", true.into()),
            ("kind", serde_json::json!({"voice": true})),
        ];
        for (field, value) in cases {
            let bytes = envelope_with_ref_fields(&r, &[(field, value.clone())]);
            let got = parse_one(&bytes);
            match field {
                "duration_ms" => assert_eq!(got.duration_ms, None, "{value}"),
                "thumb" => assert_eq!(got.thumb, None, "{value}"),
                "waveform" => assert_eq!(got.waveform, None, "{value}"),
                "width" => assert_eq!(got.width, None, "{value}"),
                "height" => assert_eq!(got.height, None, "{value}"),
                // The kind itself was the broken field; its loss also
                // drops the (voice-only) duration.
                "kind" => assert_eq!((got.kind, got.duration_ms), (None, None), "{value}"),
                _ => unreachable!(),
            }
            assert!(
                matches!(parse_body(&bytes), MessageBody::Chat { ref text, ref attachments }
                    if text == "caption" && attachments.len() == 1),
                "parse_body fell back on {field}={value}"
            );
        }
        // A wrongly typed thumb dimension drops only that dimension.
        let mut thumb = serde_json::to_value(valid_thumb()).unwrap();
        thumb["height"] = "tall".into();
        let got = parse_one(&envelope_with_ref_fields(&r, &[("thumb", thumb)]));
        assert_eq!(
            got.thumb.map(|t| (t.width, t.height)),
            Some((Some(240), None))
        );
    }

    #[test]
    fn unknown_kind_survives_as_other() {
        let r = plain_ref("application/octet-stream", "s.bin").with_kind("sticker");
        let got = parse_one(&compose("caption", vec![r]).unwrap());
        assert_eq!(got.kind.as_deref(), Some("sticker"));
        assert_eq!(got.media_kind(), Some(MediaKind::Other));
        assert_eq!(MediaKind::from_wire("Voice"), MediaKind::Other);
        assert_eq!(MediaKind::Other.as_wire(), None);
        for k in [MediaKind::Voice, MediaKind::VideoNote, MediaKind::Gif] {
            assert_eq!(MediaKind::from_wire(k.as_wire().unwrap()), k);
        }
    }

    #[test]
    fn inline_playback_is_gated_on_the_mime_allowlist_only() {
        use InlineMediaType::{Audio, Image, Video};
        let table: [(&str, Option<InlineMediaType>); 14] = [
            ("audio/mp4", Some(Audio)),
            ("audio/mp4;codecs=mp4a.40.2", Some(Audio)),
            ("AUDIO/MP4", Some(Audio)),
            ("video/mp4; codecs=\"avc1.42E01E, mp4a.40.2\"", Some(Video)),
            ("image/gif", Some(Image)),
            ("image/webp", Some(Image)),
            ("image/jpeg", Some(Image)),
            ("image/png", Some(Image)),
            ("image/svg+xml", None),
            ("text/html", None),
            ("audio/webm", None),
            ("video/mp4x", None),
            ("", None),
            ("audio/mp4x;codecs=mp4a", None),
        ];
        for (mime, want) in table {
            assert_eq!(inline_media_type(mime), want, "{mime:?}");
        }
        // kind never grants playback: a `voice` served as HTML is a file.
        let spoofed = plain_ref("text/html", "v.m4a").with_kind(MEDIA_KIND_VOICE);
        assert!(!spoofed.is_inline_playable());
        let mut no_mime = plain_ref("audio/mp4", "v.m4a").with_kind(MEDIA_KIND_VOICE);
        no_mime.mime = None;
        assert!(!no_mime.is_inline_playable());
        // …and a plain file with an allowlisted MIME still plays.
        assert_eq!(
            plain_ref("video/mp4", "clip.mp4").inline_media_type(),
            Some(Video)
        );
    }

    #[test]
    fn compose_drops_out_of_bound_metadata_before_the_wire() {
        let mut thumb = valid_thumb();
        thumb.size = MAX_THUMB_BYTES + 1;
        let r = plain_ref("audio/mp4", "v.m4a")
            .with_kind(MEDIA_KIND_VOICE)
            .with_duration_ms(MAX_VOICE_DURATION_MS + 1)
            .with_waveform(&[1u8; MAX_WAVEFORM_BYTES + 1])
            .with_dimensions(0, MAX_MEDIA_DIMENSION + 1)
            .with_thumb(thumb);
        let s = String::from_utf8(compose("c", vec![r]).unwrap()).unwrap();
        for field in ["duration_ms", "waveform", "width", "height", "thumb"] {
            assert!(!s.contains(field), "{field} went on the wire: {s}");
        }
        assert!(s.contains(r#""kind":"voice""#));
    }

    #[test]
    fn a_decoder_without_the_media_fields_still_reads_the_ref() {
        // The pre-ADR-0047 shapes, verbatim: no media fields, no
        // `deny_unknown_fields`. An old client must keep the attachment.
        #[derive(Deserialize)]
        struct LegacyRef {
            blob_id: String,
            key: String,
            nonce: String,
            #[serde(default)]
            mime: Option<String>,
            size: u64,
            #[serde(default)]
            name: Option<String>,
        }
        #[derive(Deserialize)]
        struct LegacyEnvelope {
            v: u8,
            #[serde(default)]
            text: String,
            #[serde(default)]
            attachments: Vec<LegacyRef>,
        }
        let r = plain_ref("video/mp4", "note-1.mp4")
            .with_kind(MEDIA_KIND_VIDEO_NOTE)
            .with_duration_ms(9000)
            .with_dimensions(480, 480)
            .with_thumb(valid_thumb());
        let bytes = compose("caption", vec![r.clone()]).unwrap();
        let legacy: LegacyEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((legacy.v, legacy.text.as_str()), (WIRE_VERSION, "caption"));
        let got = &legacy.attachments[0];
        assert_eq!(
            (
                &got.blob_id,
                &got.key,
                &got.nonce,
                &got.mime,
                got.size,
                &got.name
            ),
            (&r.blob_id, &r.key, &r.nonce, &r.mime, r.size, &r.name)
        );
    }

    #[test]
    fn fuzz_oracle_holds_on_hostile_media_envelopes() {
        let r = plain_ref("audio/mp4", "v.m4a").with_kind(MEDIA_KIND_VOICE);
        let hostile = [
            envelope_with_ref_fields(&r, &[("duration_ms", u32::MAX.into())]),
            envelope_with_ref_fields(&r, &[("thumb", 5.into()), ("width", 99_999.into())]),
            envelope_with_ref_fields(&r, &[("waveform", B64.encode([0u8; 400]).into())]),
            b"not json".to_vec(),
            Vec::new(),
        ];
        for bytes in hostile {
            assert!(fuzz_api::media_metadata_within_bounds(&bytes));
        }
    }

    /// The history-bundle route (`RawMediaMetadata::bounded`) keeps exactly
    /// what the envelope route (`AttachmentRef` deserialization) keeps.
    #[test]
    fn raw_media_bounds_match_the_envelope_bounds() {
        let blob = B64.encode([1u8; 32]);
        let key = B64.encode([2u8; 32]);
        let nonce = B64.encode([3u8; 12]);
        let cases: Vec<RawMediaMetadata> = vec![
            RawMediaMetadata {
                kind: Some("voice".into()),
                duration_ms: Some(MAX_VOICE_DURATION_MS + 1),
                waveform: Some(vec![9; MAX_WAVEFORM_BYTES + 1]),
                width: Some(0),
                height: Some(MAX_MEDIA_DIMENSION),
                thumb: None,
            },
            RawMediaMetadata {
                kind: Some("video_note".into()),
                duration_ms: Some(MAX_VIDEO_NOTE_DURATION_MS),
                waveform: Some(vec![]),
                width: Some(480),
                height: Some(MAX_MEDIA_DIMENSION + 1),
                thumb: Some(RawThumbMetadata {
                    size: MAX_THUMB_BYTES + 1,
                    mime: Some("image/jpeg".into()),
                    width: Some(1),
                    height: Some(1),
                }),
            },
            RawMediaMetadata {
                kind: Some("gif".into()),
                duration_ms: Some(1000),
                waveform: Some(vec![5; 3]),
                width: Some(320),
                height: Some(240),
                thumb: Some(RawThumbMetadata {
                    size: 10,
                    mime: None,
                    width: Some(MAX_MEDIA_DIMENSION + 1),
                    height: Some(20),
                }),
            },
            RawMediaMetadata::default(),
        ];
        for raw in cases {
            let mut json = serde_json::json!({
                "blob_id": blob, "key": key, "nonce": nonce, "size": 1,
            });
            let obj = json.as_object_mut().expect("object");
            if let Some(k) = &raw.kind {
                obj.insert("kind".into(), k.clone().into());
            }
            if let Some(d) = raw.duration_ms {
                obj.insert("duration_ms".into(), d.into());
            }
            if let Some(w) = &raw.waveform {
                obj.insert("waveform".into(), B64.encode(w).into());
            }
            if let Some(w) = raw.width {
                obj.insert("width".into(), w.into());
            }
            if let Some(h) = raw.height {
                obj.insert("height".into(), h.into());
            }
            if let Some(t) = &raw.thumb {
                let mut thumb = serde_json::json!({
                    "blob_id": blob, "key": key, "nonce": nonce, "size": t.size,
                });
                let tobj = thumb.as_object_mut().expect("object");
                if let Some(m) = &t.mime {
                    tobj.insert("mime".into(), m.clone().into());
                }
                if let Some(w) = t.width {
                    tobj.insert("width".into(), w.into());
                }
                if let Some(h) = t.height {
                    tobj.insert("height".into(), h.into());
                }
                obj.insert("thumb".into(), thumb);
            }
            let via_envelope: AttachmentRef = serde_json::from_value(json).expect("ref parses");
            let via_history = raw.clone().bounded();

            assert_eq!(via_history.kind, via_envelope.kind, "{raw:?}");
            assert_eq!(via_history.duration_ms, via_envelope.duration_ms, "{raw:?}");
            assert_eq!(
                via_history.waveform,
                via_envelope.decode_waveform(),
                "{raw:?}"
            );
            assert_eq!(via_history.width, via_envelope.width, "{raw:?}");
            assert_eq!(via_history.height, via_envelope.height, "{raw:?}");
            assert_eq!(
                via_history
                    .thumb
                    .as_ref()
                    .map(|t| (t.size, t.mime.clone(), t.width, t.height)),
                via_envelope
                    .thumb
                    .as_ref()
                    .map(|t| (t.size, t.mime.clone(), t.width, t.height)),
                "{raw:?}"
            );
        }
    }
}
