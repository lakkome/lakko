// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for what a group or channel says about itself
//! under ADR-0087: the group photo and allowed-reactions envelopes
//! (sections 18.1 and 13a), the conversation-handle envelope that carries a
//! group's `@name` (section 3) and the discovery handle certificate it cites.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/conversation_meta.rs`.
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`
//! (`groupPhoto.avatar.notSquare`, `conversationHandle.badSignature`, …),
//! the device-card pattern.

use lakko_attest::handle_cert::{
    verify_handle_binding as rust_verify_handle_binding, HandleCertificate, HANDLE_CERT_LEN,
};
use lakko_contacts::allowed_reactions::{
    AllowedReactions as RustAllowedReactions, ALLOWED_REACTIONS_MAX,
};
use lakko_contacts::conversation_handle::{
    compose_conversation_handle as rust_compose_conversation_handle,
    conversation_handle_is_newer as rust_conversation_handle_is_newer,
    decode_conversation_handle as rust_decode_conversation_handle, ConversationHandle,
    ConversationHandleError, CONVERSATION_HANDLE_CERT_LEN, CONVERSATION_HANDLE_ENVELOPE_MAX_LEN,
    CONVERSATION_HANDLE_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_photo::{
    compose_group_photo as rust_compose_group_photo, decode_group_photo as rust_decode_group_photo,
    group_photo_is_newer as rust_group_photo_is_newer, GroupPhoto as RustGroupPhoto,
    GroupPhotoError, GROUP_PHOTO_ENVELOPE_MAX_LEN, GROUP_PHOTO_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_reactions::{
    compose_group_reactions as rust_compose_group_reactions,
    decode_group_reactions as rust_decode_group_reactions,
    group_reactions_is_newer as rust_group_reactions_is_newer,
    GroupReactions as RustGroupReactions, GroupReactionsError, GROUP_REACTIONS_ENVELOPE_MAX_LEN,
    GROUP_REACTIONS_ENVELOPE_MIN_LEN,
};
use lakko_crypto::signing::VerifyingKey;
use wasm_bindgen::prelude::*;

use crate::contacts::ProfileAvatar;

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn bytes32(area: &str, what: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        format!(
            "{area}.badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        )
    })
}

// ============================================================================
// Allowed reactions
// ============================================================================

/// Which reactions a group's messages or a channel's posts take (ADR-0087
/// section 13a). Advisory: it decides what a client offers and shows.
#[wasm_bindgen(js_name = AllowedReactions)]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WasmAllowedReactions {
    pub(crate) inner: RustAllowedReactions,
}

#[wasm_bindgen(js_class = AllowedReactions)]
impl WasmAllowedReactions {
    /// Any reaction (the default).
    #[wasm_bindgen]
    #[must_use]
    pub fn all() -> Self {
        Self {
            inner: RustAllowedReactions::All,
        }
    }

    /// No reactions.
    #[wasm_bindgen]
    #[must_use]
    pub fn off() -> Self {
        Self {
            inner: RustAllowedReactions::Off,
        }
    }

    /// Only these emoji, in the order a composer offers them.
    ///
    /// # Errors
    ///
    /// Throws `allowedReactions.<code>: ...` for an empty list, a repeat, an
    /// entry that is not a reaction emoji, or more than
    /// `allowedReactionsMax()`.
    #[wasm_bindgen]
    pub fn only(emoji: Vec<String>) -> Result<WasmAllowedReactions, String> {
        let inner = RustAllowedReactions::Only(emoji);
        inner
            .validate()
            .map_err(|e| format!("allowedReactions.{}: {e}", e.code()))?;
        Ok(Self { inner })
    }

    /// `"all"`, `"off"` or `"only"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn mode(&self) -> String {
        match self.inner {
            RustAllowedReactions::All => "all",
            RustAllowedReactions::Off => "off",
            RustAllowedReactions::Only(_) => "only",
        }
        .to_string()
    }

    /// The list of an `"only"` value; empty otherwise.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn emoji(&self) -> Vec<String> {
        match &self.inner {
            RustAllowedReactions::Only(list) => list.clone(),
            _ => Vec::new(),
        }
    }

    /// Whether `emoji` is allowed.
    #[wasm_bindgen]
    #[must_use]
    pub fn allows(&self, emoji: &str) -> bool {
        self.inner.allows(emoji)
    }
}

/// The longest "only these" list: 100.
#[wasm_bindgen(js_name = allowedReactionsMax)]
#[must_use]
pub fn allowed_reactions_max() -> u32 {
    len_u32(ALLOWED_REACTIONS_MAX)
}

// ============================================================================
// Group photo envelope
// ============================================================================

/// A group's photo as an owner or admin set it (ADR-0087 section 18.1).
#[wasm_bindgen(js_name = GroupPhoto)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmGroupPhoto {
    pub(crate) inner: RustGroupPhoto,
}

impl From<RustGroupPhoto> for WasmGroupPhoto {
    fn from(inner: RustGroupPhoto) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = GroupPhoto)]
impl WasmGroupPhoto {
    /// A photo (`avatar` undefined = remove it) stamped `updatedAtMs`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(avatar: Option<ProfileAvatar>, updated_at_ms: u64) -> Self {
        Self {
            inner: RustGroupPhoto {
                avatar: avatar.map(|a| a.inner),
                updated_at_ms,
            },
        }
    }

    /// The photo, or `undefined` for none. Its keys are SECRET.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn avatar(&self) -> Option<ProfileAvatar> {
        self.inner
            .avatar
            .clone()
            .map(|inner| ProfileAvatar { inner })
    }

    /// Setter-clock UNIX milliseconds.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }
}

fn group_photo_error(e: GroupPhotoError) -> String {
    format!("groupPhoto.{}: {e}", e.code())
}

/// Compose a group-photo envelope.
///
/// # Errors
///
/// Throws `groupPhoto.<code>: ...`.
#[wasm_bindgen(js_name = composeGroupPhotoEnvelope)]
pub fn compose_group_photo_envelope(photo: &WasmGroupPhoto) -> Result<Vec<u8>, String> {
    rust_compose_group_photo(&photo.inner).map_err(group_photo_error)
}

/// Strictly decode a group-photo envelope.
///
/// # Errors
///
/// Throws `groupPhoto.<code>: ...`.
#[wasm_bindgen(js_name = decodeGroupPhotoEnvelope)]
pub fn decode_group_photo_envelope(plaintext: &[u8]) -> Result<WasmGroupPhoto, String> {
    rust_decode_group_photo(plaintext)
        .map(Into::into)
        .map_err(group_photo_error)
}

/// `true` when `incoming` replaces `stored` for the same group.
#[wasm_bindgen(js_name = groupPhotoIsNewer)]
#[must_use]
pub fn group_photo_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_photo_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest group-photo envelope.
#[wasm_bindgen(js_name = groupPhotoEnvelopeMinLen)]
#[must_use]
pub fn group_photo_envelope_min_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MIN_LEN)
}

/// Longest group-photo envelope.
#[wasm_bindgen(js_name = groupPhotoEnvelopeMaxLen)]
#[must_use]
pub fn group_photo_envelope_max_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Group allowed-reactions envelope
// ============================================================================

/// A group's allowed reactions as an owner or admin set them.
#[wasm_bindgen(js_name = GroupReactions)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmGroupReactions {
    pub(crate) inner: RustGroupReactions,
}

impl From<RustGroupReactions> for WasmGroupReactions {
    fn from(inner: RustGroupReactions) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = GroupReactions)]
impl WasmGroupReactions {
    /// The value stamped `updatedAtMs`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(reactions: &WasmAllowedReactions, updated_at_ms: u64) -> Self {
        Self {
            inner: RustGroupReactions {
                reactions: reactions.inner.clone(),
                updated_at_ms,
            },
        }
    }

    /// What the group's messages take.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn reactions(&self) -> WasmAllowedReactions {
        WasmAllowedReactions {
            inner: self.inner.reactions.clone(),
        }
    }

    /// Setter-clock UNIX milliseconds.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }
}

fn group_reactions_error(e: GroupReactionsError) -> String {
    format!("groupReactions.{}: {e}", e.code())
}

/// Compose a group allowed-reactions envelope.
///
/// # Errors
///
/// Throws `groupReactions.<code>: ...`.
#[wasm_bindgen(js_name = composeGroupReactionsEnvelope)]
pub fn compose_group_reactions_envelope(value: &WasmGroupReactions) -> Result<Vec<u8>, String> {
    value
        .inner
        .reactions
        .validate()
        .map_err(|e| group_reactions_error(e.into()))?;
    rust_compose_group_reactions(&value.inner).map_err(group_reactions_error)
}

/// Strictly decode a group allowed-reactions envelope.
///
/// # Errors
///
/// Throws `groupReactions.<code>: ...`.
#[wasm_bindgen(js_name = decodeGroupReactionsEnvelope)]
pub fn decode_group_reactions_envelope(plaintext: &[u8]) -> Result<WasmGroupReactions, String> {
    rust_decode_group_reactions(plaintext)
        .map(Into::into)
        .map_err(group_reactions_error)
}

/// `true` when `incoming` replaces `stored` for the same group.
#[wasm_bindgen(js_name = groupReactionsIsNewer)]
#[must_use]
pub fn group_reactions_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_reactions_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest group allowed-reactions envelope.
#[wasm_bindgen(js_name = groupReactionsEnvelopeMinLen)]
#[must_use]
pub fn group_reactions_envelope_min_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MIN_LEN)
}

/// Longest group allowed-reactions envelope.
#[wasm_bindgen(js_name = groupReactionsEnvelopeMaxLen)]
#[must_use]
pub fn group_reactions_envelope_max_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Conversation handle envelope
// ============================================================================

/// A group's (or, inside its metadata record, a channel's) `@name`, its
/// public / private flag and the handle key's signature (ADR-0087 section 3).
#[wasm_bindgen(js_name = ConversationHandle)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmConversationHandle {
    pub(crate) inner: ConversationHandle,
}

impl From<ConversationHandle> for WasmConversationHandle {
    fn from(inner: ConversationHandle) -> Self {
        Self { inner }
    }
}

fn conversation_handle_error(e: ConversationHandleError) -> String {
    format!("conversationHandle.{}: {e}", e.code())
}

#[wasm_bindgen(js_class = ConversationHandle)]
impl WasmConversationHandle {
    /// Build and sign with the 32-byte handle seed for `groupId` (a channel:
    /// its PublisherGroup id).
    ///
    /// # Errors
    ///
    /// Throws `conversationHandle.<code>: ...`.
    #[wasm_bindgen]
    pub fn sign(
        handle_seed: &[u8],
        group_id: &[u8],
        updated_at_ms: u64,
        public: bool,
        handle: &str,
        certificate: &[u8],
    ) -> Result<WasmConversationHandle, String> {
        let seed = bytes32("conversationHandle", "handle seed", handle_seed)?;
        let certificate = <[u8; CONVERSATION_HANDLE_CERT_LEN]>::try_from(certificate).map_err(|_| {
            format!(
                "conversationHandle.badLength: certificate must be {CONVERSATION_HANDLE_CERT_LEN} bytes"
            )
        })?;
        ConversationHandle::sign(&seed, group_id, updated_at_ms, public, handle, certificate)
            .map(Into::into)
            .map_err(conversation_handle_error)
    }

    /// Owner-clock UNIX milliseconds of the change.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }

    /// `true` for a public group or channel.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn public(&self) -> bool {
        self.inner.public
    }

    /// The canonical `@name`, without `@`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn handle(&self) -> String {
        self.inner.handle.clone()
    }

    /// The 169-byte discovery handle certificate.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn certificate(&self) -> Vec<u8> {
        self.inner.certificate.to_vec()
    }

    /// Verify the signature for `groupId` under the 32-byte `handleKey` (the
    /// write key the verified certificate names).
    ///
    /// # Errors
    ///
    /// Throws `conversationHandle.badSignature: ...` (or a length code).
    #[wasm_bindgen]
    pub fn verify(&self, group_id: &[u8], handle_key: &[u8]) -> Result<(), String> {
        let key = bytes32("conversationHandle", "handle key", handle_key)?;
        self.inner
            .verify_signature(group_id, &key)
            .map_err(conversation_handle_error)
    }
}

/// Compose a conversation-handle envelope for the group's journal.
///
/// # Errors
///
/// Throws `conversationHandle.<code>: ...`.
#[wasm_bindgen(js_name = composeConversationHandleEnvelope)]
pub fn compose_conversation_handle_envelope(
    value: &WasmConversationHandle,
) -> Result<Vec<u8>, String> {
    rust_compose_conversation_handle(&value.inner).map_err(conversation_handle_error)
}

/// Strictly decode a conversation-handle envelope (the signature is not
/// checked here: `verifyHandleBinding`, then `ConversationHandle.verify`).
///
/// # Errors
///
/// Throws `conversationHandle.<code>: ...`.
#[wasm_bindgen(js_name = decodeConversationHandleEnvelope)]
pub fn decode_conversation_handle_envelope(
    plaintext: &[u8],
) -> Result<WasmConversationHandle, String> {
    rust_decode_conversation_handle(plaintext)
        .map(Into::into)
        .map_err(conversation_handle_error)
}

/// `true` when `incomingMs` replaces `storedMs`.
#[wasm_bindgen(js_name = conversationHandleIsNewer)]
#[must_use]
pub fn conversation_handle_is_newer(incoming_ms: u64, stored_ms: u64) -> bool {
    rust_conversation_handle_is_newer(incoming_ms, stored_ms)
}

/// Shortest conversation-handle envelope.
#[wasm_bindgen(js_name = conversationHandleEnvelopeMinLen)]
#[must_use]
pub fn conversation_handle_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MIN_LEN)
}

/// Longest conversation-handle envelope.
#[wasm_bindgen(js_name = conversationHandleEnvelopeMaxLen)]
#[must_use]
pub fn conversation_handle_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Handle certificate (verify only)
// ============================================================================

/// The authenticated claims of a verified handle certificate.
#[wasm_bindgen(js_name = HandleCertificate)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmHandleCertificate {
    inner: HandleCertificate,
}

#[wasm_bindgen(js_class = HandleCertificate)]
impl WasmHandleCertificate {
    /// The certified 64-byte lookup key of the name.
    #[wasm_bindgen(getter, js_name = lookupKey)]
    #[must_use]
    pub fn lookup_key(&self) -> Vec<u8> {
        self.inner.lookup_key().to_vec()
    }

    /// The 32-byte handle key that holds the name.
    #[wasm_bindgen(getter, js_name = writePk)]
    #[must_use]
    pub fn write_pk(&self) -> Vec<u8> {
        self.inner.write_pk().to_vec()
    }

    /// When discovery issued it, UNIX ms. Informational.
    #[wasm_bindgen(getter, js_name = issuedAtUnixMillis)]
    #[must_use]
    pub fn issued_at_unix_millis(&self) -> i64 {
        self.inner.issued_at_unix_millis()
    }
}

/// Verify a handle certificate under the pinned discovery key and its
/// binding to `expectedLookupKey` (the lookup key of the OPRF output over
/// the SHOWN name).
///
/// # Errors
///
/// Throws `"<code>: ..."` with `badLength` / `badVersion` / `badSignature`
/// / `nameMismatch`, or `badVerifyingKey` / `lookupKeyLength`.
#[wasm_bindgen(js_name = verifyHandleBinding)]
pub fn verify_handle_binding(
    certificate: &[u8],
    verifying_key: &[u8],
    expected_lookup_key: &[u8],
) -> Result<WasmHandleCertificate, String> {
    let verifying = VerifyingKey::from_bytes(verifying_key).map_err(|_| {
        "badVerifyingKey: the pinned discovery key is not an Ed25519 key".to_string()
    })?;
    let lookup = <[u8; 64]>::try_from(expected_lookup_key).map_err(|_| {
        format!(
            "lookupKeyLength: lookup key must be 64 bytes, got {}",
            expected_lookup_key.len()
        )
    })?;
    rust_verify_handle_binding(certificate, &verifying, &lookup)
        .map(|inner| WasmHandleCertificate { inner })
        .map_err(|e| format!("{}: {e}", e.code()))
}

/// Wire length of a handle certificate: 169.
#[wasm_bindgen(js_name = handleCertificateLen)]
#[must_use]
pub fn handle_certificate_len() -> u32 {
    len_u32(HANDLE_CERT_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn group_photo_and_reactions_round_trip() {
        let photo = WasmGroupPhoto::new(None, 7);
        let wire = compose_group_photo_envelope(&photo).unwrap();
        assert_eq!(decode_group_photo_envelope(&wire).unwrap(), photo);
        assert!(group_photo_is_newer(8, 7));
        assert!(decode_group_photo_envelope(b"x")
            .unwrap_err()
            .starts_with("groupPhoto.wrongDomain"));
        let only = WasmAllowedReactions::only(vec!["\u{1F44D}".into()]).unwrap();
        assert_eq!(only.mode(), "only");
        let value = WasmGroupReactions::new(&only, 9);
        let wire = compose_group_reactions_envelope(&value).unwrap();
        assert_eq!(decode_group_reactions_envelope(&wire).unwrap(), value);
        assert!(WasmAllowedReactions::only(Vec::new()).is_err());
        assert!(!WasmAllowedReactions::off().allows("\u{1F44D}"));
        assert!(wire.len() <= group_reactions_envelope_max_len() as usize);
    }

    #[test]
    fn conversation_handle_and_certificate() {
        let group = [0x42_u8; 32];
        let handle_key = SigningKey::from_seed(&[0x21; 32])
            .verifying_key()
            .to_bytes();
        let h = WasmConversationHandle::sign(&[0x21; 32], &group, 5, true, "my_group", &[0; 169])
            .unwrap();
        let wire = compose_conversation_handle_envelope(&h).unwrap();
        let back = decode_conversation_handle_envelope(&wire).unwrap();
        assert_eq!(back, h);
        back.verify(&group, &handle_key).unwrap();
        assert!(back.verify(&[1; 32], &handle_key).is_err());
        let discovery = SigningKey::from_seed(&[0x31; 32]);
        let cert = HandleCertificate::new([0xAB; 64], [0xCD; 32], 11).sign(&discovery);
        let vk = discovery.verifying_key().to_bytes();
        let got = verify_handle_binding(&cert, &vk, &[0xAB; 64]).unwrap();
        assert_eq!(got.write_pk(), vec![0xCD; 32]);
        assert!(verify_handle_binding(&cert, &vk, &[0xAC; 64])
            .unwrap_err()
            .starts_with("nameMismatch"));
    }
}
