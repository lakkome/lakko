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
    let inner_refs: Vec<inner::AttachmentRef> = attachments
        .into_iter()
        .map(|r| -> Result<inner::AttachmentRef, FfiError> {
            // Validate field lengths up front so a wrong-shape
            // input fails fast at the FFI rather than at JSON
            // parse on the other side.
            if r.blob_id.len() != inner::BLOB_ID_LEN {
                return Err(FfiError::Attachment(format!(
                    "blob_id length {} != {}",
                    r.blob_id.len(),
                    inner::BLOB_ID_LEN
                )));
            }
            if r.key.len() != inner::ATTACHMENT_KEY_LEN {
                return Err(FfiError::Attachment(format!(
                    "key length {} != {}",
                    r.key.len(),
                    inner::ATTACHMENT_KEY_LEN
                )));
            }
            if r.nonce.len() != inner::ATTACHMENT_NONCE_LEN {
                return Err(FfiError::Attachment(format!(
                    "nonce length {} != {}",
                    r.nonce.len(),
                    inner::ATTACHMENT_NONCE_LEN
                )));
            }
            Ok(inner::AttachmentRef {
                blob_id: B64.encode(&r.blob_id),
                key: B64.encode(&r.key),
                nonce: B64.encode(&r.nonce),
                mime: r.mime,
                size: r.size,
                name: r.name,
            })
        })
        .collect::<Result<_, _>>()?;
    inner::compose(&text, inner_refs).map_err(Into::into)
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
    ParsedEnvelope {
        text,
        attachments: refs
            .into_iter()
            .filter_map(|r| {
                // Skip malformed refs silently rather than failing
                // the whole envelope parse — the rest of the
                // attachments may still be useful, and the
                // application path can decide whether a
                // count-mismatch is a user-visible issue.
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
            .collect(),
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

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
            key: vec![0u8; 32],
            nonce: vec![0u8; 12],
            mime: None,
            size: 0,
            name: None,
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
}
