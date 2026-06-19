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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl AttachmentRef {
    /// Build a wire ref from an [`EncryptedAttachment`] + the
    /// caller-supplied metadata (mime / name / size).
    ///
    /// `size` should be the **plaintext** size — that's what the
    /// UI surfaces to the user; ciphertext is plaintext + 16-byte
    /// tag (negligible delta but conceptually the wire-friendly
    /// choice is plaintext).
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
        }
    }

    /// Decode the base64-encoded `blob_id` into 32 raw bytes.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] if the base64 doesn't decode
    /// or the decoded length is not [`BLOB_ID_LEN`].
    pub fn decode_blob_id(&self) -> Result<[u8; BLOB_ID_LEN], AttachmentError> {
        let bytes = B64
            .decode(&self.blob_id)
            .map_err(|e| AttachmentError::WireFormat(format!("blob_id base64: {e}")))?;
        bytes.try_into().map_err(|v: Vec<u8>| {
            AttachmentError::WireFormat(format!("blob_id length {} != {BLOB_ID_LEN}", v.len()))
        })
    }

    /// Decode the base64-encoded CEK into a zeroize-on-drop wrapper.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_key(&self) -> Result<AeadKey, AttachmentError> {
        let bytes = B64
            .decode(&self.key)
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

    /// Decode the base64-encoded nonce.
    ///
    /// # Errors
    ///
    /// [`AttachmentError::WireFormat`] on bad base64 / wrong length.
    pub fn decode_nonce(&self) -> Result<[u8; NONCE_LEN], AttachmentError> {
        let bytes = B64
            .decode(&self.nonce)
            .map_err(|e| AttachmentError::WireFormat(format!("nonce base64: {e}")))?;
        bytes.try_into().map_err(|v: Vec<u8>| {
            AttachmentError::WireFormat(format!("nonce length {} != {NONCE_LEN}", v.len()))
        })
    }
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
/// # Errors
///
/// [`AttachmentError::WireFormat`] if `serde_json::to_vec` fails
/// (essentially impossible for valid inputs).
pub fn compose(text: &str, attachments: Vec<AttachmentRef>) -> Result<Vec<u8>, AttachmentError> {
    let envelope = Envelope {
        text: text.to_string(),
        attachments,
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
    use super::{parse, parse_body, MessageBody};

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
}
