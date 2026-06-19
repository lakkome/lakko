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
#[wasm_bindgen]
pub struct AttachmentRef {
    blob_id: Vec<u8>,
    key: Vec<u8>,
    nonce: Vec<u8>,
    mime: Option<String>,
    size: u64,
    name: Option<String>,
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
        }
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
            .map(|a| AttachmentRef {
                blob_id: a.blob_id.clone(),
                key: a.key.clone(),
                nonce: a.nonce.clone(),
                mime: a.mime.clone(),
                size: a.size,
                name: a.name.clone(),
            })
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
#[wasm_bindgen]
pub struct EnvelopeBuilder {
    text: String,
    refs: Vec<inner::AttachmentRef>,
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
        }
    }

    /// Append one attachment ref. Validates the byte-field lengths
    /// up front — wrong-length `blobId` / `key` / `nonce` throws
    /// immediately, not at build-time.
    #[wasm_bindgen]
    pub fn add(&mut self, r: &AttachmentRef) -> Result<(), JsError> {
        if r.blob_id.len() != inner::BLOB_ID_LEN {
            return Err(boundary_err(format!(
                "blob_id length {} != {}",
                r.blob_id.len(),
                inner::BLOB_ID_LEN
            )));
        }
        if r.key.len() != inner::ATTACHMENT_KEY_LEN {
            return Err(boundary_err(format!(
                "key length {} != {}",
                r.key.len(),
                inner::ATTACHMENT_KEY_LEN
            )));
        }
        if r.nonce.len() != inner::ATTACHMENT_NONCE_LEN {
            return Err(boundary_err(format!(
                "nonce length {} != {}",
                r.nonce.len(),
                inner::ATTACHMENT_NONCE_LEN
            )));
        }
        self.refs.push(inner::AttachmentRef {
            blob_id: B64.encode(&r.blob_id),
            key: B64.encode(&r.key),
            nonce: B64.encode(&r.nonce),
            mime: r.mime.clone(),
            size: r.size,
            name: r.name.clone(),
        });
        Ok(())
    }

    /// Consume the builder + emit the JSON envelope bytes. Hand
    /// the result to `Group.createApplicationMessage(identity,
    /// bytes)`.
    #[wasm_bindgen]
    pub fn build(self) -> Result<Vec<u8>, JsError> {
        inner::compose(&self.text, self.refs).map_err(attachment_err)
    }
}

/// Convenience for the no-attachment case: build a text-only
/// envelope without going through [`EnvelopeBuilder`].
#[wasm_bindgen(js_name = composeEnvelopeText)]
pub fn compose_envelope_text(text: &str) -> Result<Vec<u8>, JsError> {
    inner::compose(text, Vec::new()).map_err(attachment_err)
}

/// Decode an MLS Application message JSON envelope. Mirror of
/// `lakko_attachments::parse` — never fails; legacy / malformed
/// inputs collapse into the documented fallback paths.
#[wasm_bindgen(js_name = parseEnvelope)]
#[must_use]
pub fn parse_envelope(bytes: &[u8]) -> ParsedEnvelope {
    let (text, refs) = inner::parse(bytes);
    let attachments = refs
        .into_iter()
        .filter_map(|r| {
            // Same skip-malformed-silently posture as the UniFFI
            // side. Receivers see the rest of the attachments;
            // the application path decides whether a count
            // mismatch is worth surfacing to the user.
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
        .collect();
    ParsedEnvelope { text, attachments }
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
}

#[wasm_bindgen]
impl ParsedBody {
    /// `"chat" | "edit" | "delete" | "reaction" | "typing" | "read-receipt"`.
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

    /// The DS `messageId` the op acts on, or `undefined` for `"chat"`.
    #[wasm_bindgen(getter)]
    pub fn target(&self) -> Option<i64> {
        self.target
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
            .map(|a| AttachmentRef {
                blob_id: a.blob_id.clone(),
                key: a.key.clone(),
                nonce: a.nonce.clone(),
                mime: a.mime.clone(),
                size: a.size,
                name: a.name.clone(),
            })
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
#[wasm_bindgen(js_name = composeReactionEnvelope)]
pub fn compose_reaction_envelope(target: i64, emoji: &str) -> Result<Vec<u8>, JsError> {
    inner::compose_reaction(target, emoji).map_err(attachment_err)
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
pub fn parse_body_envelope(bytes: &[u8]) -> ParsedBody {
    match inner::parse_body(bytes) {
        inner::MessageBody::Chat { text, attachments } => ParsedBody {
            kind: "chat",
            text,
            attachments: refs_to_wasm(attachments),
            target: None,
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Edit { target, text } => ParsedBody {
            kind: "edit",
            text,
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Delete { target } => ParsedBody {
            kind: "delete",
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: None,
            is_typing: None,
        },
        inner::MessageBody::Reaction { target, emoji } => ParsedBody {
            kind: "reaction",
            text: String::new(),
            attachments: Vec::new(),
            target: Some(target),
            emoji: Some(emoji),
            is_typing: None,
        },
        inner::MessageBody::Typing { is_typing } => ParsedBody {
            kind: "typing",
            text: String::new(),
            attachments: Vec::new(),
            target: None,
            emoji: None,
            is_typing: Some(is_typing),
        },
        inner::MessageBody::ReadReceipt { up_to } => ParsedBody {
            kind: "read-receipt",
            text: String::new(),
            attachments: Vec::new(),
            target: Some(up_to),
            emoji: None,
            is_typing: None,
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
    fn parse_unknown_version_surfaces_placeholder() {
        let evil = br#"{"v":99,"text":"future","attachments":[]}"#;
        let parsed = parse_envelope(evil);
        assert_eq!(parsed.text(), "[unsupported v99 message]");
        assert_eq!(parsed.attachment_count(), 0);
    }
}
