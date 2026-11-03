// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for what a group or channel says about itself under
//! ADR-0087: the group photo and allowed-reactions envelopes (sections 18.1
//! and 13a), the conversation-handle envelope that carries a group's `@name`
//! (section 3) and the discovery handle certificate it cites.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/conversation_meta.rs`. Every
//! envelope here rides an MLS application message on the group's journal; a
//! receiver classifies it with `dispatchIncomingEnvelope` (kinds
//! `GroupPhoto`, `GroupReactions`, `ConversationHandle`), admits it only from
//! an owner or admin resolved at receipt (C-68), keeps it only when newer, and
//! never renders it.

use lakko_attest::handle_cert::{
    verify_handle_binding as rust_verify_handle_binding, HandleCertError, HandleCertificate,
    HANDLE_CERT_LEN,
};
use lakko_contacts::allowed_reactions::{
    AllowedReactions, AllowedReactionsError, ALLOWED_REACTIONS_MAX,
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
    group_photo_is_newer as rust_group_photo_is_newer, GroupPhoto, GroupPhotoError,
    GROUP_PHOTO_ENVELOPE_MAX_LEN, GROUP_PHOTO_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_reactions::{
    compose_group_reactions as rust_compose_group_reactions,
    decode_group_reactions as rust_decode_group_reactions,
    group_reactions_is_newer as rust_group_reactions_is_newer, GroupReactions, GroupReactionsError,
    GROUP_REACTIONS_ENVELOPE_MAX_LEN, GROUP_REACTIONS_ENVELOPE_MIN_LEN,
};
use lakko_crypto::signing::VerifyingKey;

use crate::contacts::FfiAvatar;
use crate::FfiError;

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn bytes32(what: &str, bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Contacts(format!(
            "badLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

// ============================================================================
// Allowed reactions (ADR-0087 section 13a, UX reactions-and-emoji section 11)
// ============================================================================

/// Which reactions a group's messages or a channel's posts take. Advisory:
/// it decides what a client offers and shows, never what a peer may send.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Default)]
pub enum FfiAllowedReactions {
    /// Any reaction (the default).
    #[default]
    All,
    /// No reactions.
    Off,
    /// Only these, 1 to [`allowed_reactions_max`] distinct emoji, in the
    /// setter's order.
    Only {
        /// The emoji, in the order a composer offers them.
        emoji: Vec<String>,
    },
}

impl From<AllowedReactions> for FfiAllowedReactions {
    fn from(a: AllowedReactions) -> Self {
        match a {
            AllowedReactions::All => Self::All,
            AllowedReactions::Off => Self::Off,
            AllowedReactions::Only(emoji) => Self::Only { emoji },
        }
    }
}

impl From<FfiAllowedReactions> for AllowedReactions {
    fn from(a: FfiAllowedReactions) -> Self {
        match a {
            FfiAllowedReactions::All => Self::All,
            FfiAllowedReactions::Off => Self::Off,
            FfiAllowedReactions::Only { emoji } => Self::Only(emoji),
        }
    }
}

fn allowed_reactions_error(e: AllowedReactionsError) -> FfiError {
    FfiError::Contacts(format!("allowedReactions.{}: {e}", e.code()))
}

/// Check an allowed-reactions value the way every decoder does (a non-empty
/// list of at most [`allowed_reactions_max`] distinct reaction emoji).
///
/// # Errors
///
/// [`FfiError::Contacts`] with `allowedReactions.<code>` first.
#[uniffi::export]
pub fn validate_allowed_reactions(value: FfiAllowedReactions) -> Result<(), FfiError> {
    AllowedReactions::from(value)
        .validate()
        .map_err(allowed_reactions_error)
}

/// Whether `emoji` is allowed by `value` (advisory, UX section 11).
#[uniffi::export]
#[must_use]
pub fn allowed_reactions_allows(value: FfiAllowedReactions, emoji: String) -> bool {
    AllowedReactions::from(value).allows(&emoji)
}

/// The longest "only these" list: 100.
#[uniffi::export]
#[must_use]
pub fn allowed_reactions_max() -> u32 {
    len_u32(ALLOWED_REACTIONS_MAX)
}

// ============================================================================
// Group photo envelope (ADR-0087 section 18.1)
// ============================================================================

/// A group's photo as an owner or admin set it.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiGroupPhoto {
    /// The photo, or `None` to remove it. Its keys are SECRET to the members.
    pub avatar: Option<FfiAvatar>,
    /// Setter-clock UNIX milliseconds. A growth re-send carries the stored
    /// value verbatim.
    pub updated_at_ms: u64,
}

fn group_photo_error(e: GroupPhotoError) -> FfiError {
    FfiError::Contacts(format!("groupPhoto.{}: {e}", e.code()))
}

impl From<GroupPhoto> for FfiGroupPhoto {
    fn from(p: GroupPhoto) -> Self {
        Self {
            avatar: p.avatar.map(Into::into),
            updated_at_ms: p.updated_at_ms,
        }
    }
}

/// Compose a group-photo envelope (the plaintext of an MLS application
/// message on the group's journal).
///
/// # Errors
///
/// [`FfiError::Contacts`]: a wrong-length avatar field, or
/// `groupPhoto.avatar.<code>` for a photo out of bounds.
#[uniffi::export]
pub fn compose_group_photo_envelope(photo: FfiGroupPhoto) -> Result<Vec<u8>, FfiError> {
    let core = GroupPhoto {
        avatar: photo.avatar.map(TryInto::try_into).transpose()?,
        updated_at_ms: photo.updated_at_ms,
    };
    rust_compose_group_photo(&core).map_err(group_photo_error)
}

/// Strictly decode a group-photo envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `groupPhoto.<code>` first.
#[uniffi::export]
pub fn decode_group_photo_envelope(plaintext: Vec<u8>) -> Result<FfiGroupPhoto, FfiError> {
    rust_decode_group_photo(&plaintext)
        .map(Into::into)
        .map_err(group_photo_error)
}

/// `true` when a photo stamped `incoming_updated_at_ms` replaces the stored
/// one for the same group. Strictly greater.
#[uniffi::export]
#[must_use]
pub fn group_photo_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_photo_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest group-photo envelope (a removal).
#[uniffi::export]
#[must_use]
pub fn group_photo_envelope_min_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MIN_LEN)
}

/// Longest group-photo envelope (a photo with a video).
#[uniffi::export]
#[must_use]
pub fn group_photo_envelope_max_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Group allowed-reactions envelope (ADR-0087 section 13a)
// ============================================================================

/// A group's allowed reactions as an owner or admin set them.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiGroupReactions {
    /// What the group's messages take.
    pub reactions: FfiAllowedReactions,
    /// Setter-clock UNIX milliseconds, re-emitted verbatim on a re-send.
    pub updated_at_ms: u64,
}

fn group_reactions_error(e: GroupReactionsError) -> FfiError {
    FfiError::Contacts(format!("groupReactions.{}: {e}", e.code()))
}

impl From<GroupReactions> for FfiGroupReactions {
    fn from(g: GroupReactions) -> Self {
        Self {
            reactions: g.reactions.into(),
            updated_at_ms: g.updated_at_ms,
        }
    }
}

/// Compose a group allowed-reactions envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `groupReactions.<code>` first for a list out
/// of bounds.
#[uniffi::export]
pub fn compose_group_reactions_envelope(value: FfiGroupReactions) -> Result<Vec<u8>, FfiError> {
    let core = GroupReactions {
        reactions: value.reactions.into(),
        updated_at_ms: value.updated_at_ms,
    };
    core.reactions
        .validate()
        .map_err(|e| group_reactions_error(e.into()))?;
    rust_compose_group_reactions(&core).map_err(group_reactions_error)
}

/// Strictly decode a group allowed-reactions envelope.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `groupReactions.<code>` first.
#[uniffi::export]
pub fn decode_group_reactions_envelope(plaintext: Vec<u8>) -> Result<FfiGroupReactions, FfiError> {
    rust_decode_group_reactions(&plaintext)
        .map(Into::into)
        .map_err(group_reactions_error)
}

/// `true` when a value stamped `incoming_updated_at_ms` replaces the stored
/// one for the same group. Strictly greater.
#[uniffi::export]
#[must_use]
pub fn group_reactions_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_reactions_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest group allowed-reactions envelope.
#[uniffi::export]
#[must_use]
pub fn group_reactions_envelope_min_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MIN_LEN)
}

/// Longest group allowed-reactions envelope.
#[uniffi::export]
#[must_use]
pub fn group_reactions_envelope_max_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Conversation handle envelope (ADR-0087 section 3)
// ============================================================================

/// A group's (or, inside its metadata record, a channel's) `@name`, its
/// public / private flag and the handle key's signature.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiConversationHandle {
    /// Owner-clock UNIX milliseconds of the change. Ordered strictly.
    pub updated_at_ms: u64,
    /// `true` for a public group or channel.
    pub public: bool,
    /// The canonical `@name`, without `@`.
    pub handle: String,
    /// The 169-byte discovery handle certificate.
    pub certificate: Vec<u8>,
    /// The 64-byte Ed25519 signature by the handle key.
    pub signature: Vec<u8>,
}

fn conversation_handle_error(e: ConversationHandleError) -> FfiError {
    FfiError::Contacts(format!("conversationHandle.{}: {e}", e.code()))
}

impl From<ConversationHandle> for FfiConversationHandle {
    fn from(h: ConversationHandle) -> Self {
        Self {
            updated_at_ms: h.updated_at_ms,
            public: h.public,
            handle: h.handle,
            certificate: h.certificate.to_vec(),
            signature: h.signature.to_vec(),
        }
    }
}

impl TryFrom<FfiConversationHandle> for ConversationHandle {
    type Error = FfiError;

    fn try_from(h: FfiConversationHandle) -> Result<Self, FfiError> {
        let certificate = <[u8; CONVERSATION_HANDLE_CERT_LEN]>::try_from(h.certificate.as_slice())
            .map_err(|_| {
                FfiError::Contacts(format!(
                    "conversationHandle.badLength: certificate must be \
                     {CONVERSATION_HANDLE_CERT_LEN} bytes"
                ))
            })?;
        let signature = <[u8; 64]>::try_from(h.signature.as_slice()).map_err(|_| {
            FfiError::Contacts(
                "conversationHandle.badLength: signature must be 64 bytes".to_string(),
            )
        })?;
        Ok(Self {
            updated_at_ms: h.updated_at_ms,
            public: h.public,
            handle: h.handle,
            certificate,
            signature,
        })
    }
}

/// Build and sign a conversation-handle value with the 32-byte handle seed
/// (`handleWriteSeed(journalRootSeed)`) for `group_id`. For a channel, pass
/// the channel's PublisherGroup id.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `conversationHandle.<code>` first.
#[uniffi::export]
pub fn sign_conversation_handle(
    handle_seed: Vec<u8>,
    group_id: Vec<u8>,
    updated_at_ms: u64,
    public: bool,
    handle: String,
    certificate: Vec<u8>,
) -> Result<FfiConversationHandle, FfiError> {
    let seed = bytes32("handle seed", &handle_seed)?;
    let certificate = <[u8; CONVERSATION_HANDLE_CERT_LEN]>::try_from(certificate.as_slice())
        .map_err(|_| {
            FfiError::Contacts(format!(
                "conversationHandle.badLength: certificate must be \
                 {CONVERSATION_HANDLE_CERT_LEN} bytes"
            ))
        })?;
    ConversationHandle::sign(
        &seed,
        &group_id,
        updated_at_ms,
        public,
        &handle,
        certificate,
    )
    .map(Into::into)
    .map_err(conversation_handle_error)
}

/// Compose a conversation-handle envelope for the group's journal.
///
/// # Errors
///
/// [`FfiError::Contacts`] with `conversationHandle.<code>` first.
#[uniffi::export]
pub fn compose_conversation_handle_envelope(
    value: FfiConversationHandle,
) -> Result<Vec<u8>, FfiError> {
    rust_compose_conversation_handle(&value.try_into()?).map_err(conversation_handle_error)
}

/// Strictly decode a conversation-handle envelope. The signature is NOT
/// checked: verify the certificate with [`verify_handle_binding`] and then
/// the signature with [`verify_conversation_handle`].
///
/// # Errors
///
/// [`FfiError::Contacts`] with `conversationHandle.<code>` first.
#[uniffi::export]
pub fn decode_conversation_handle_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiConversationHandle, FfiError> {
    rust_decode_conversation_handle(&plaintext)
        .map(Into::into)
        .map_err(conversation_handle_error)
}

/// Verify the handle key's signature for `group_id` under `handle_key` (the
/// write key the verified certificate names).
///
/// # Errors
///
/// [`FfiError::Contacts`] with `conversationHandle.badSignature` (or a
/// length code) first.
#[uniffi::export]
pub fn verify_conversation_handle(
    value: FfiConversationHandle,
    group_id: Vec<u8>,
    handle_key: Vec<u8>,
) -> Result<(), FfiError> {
    let key = bytes32("handle key", &handle_key)?;
    ConversationHandle::try_from(value)?
        .verify_signature(&group_id, &key)
        .map_err(conversation_handle_error)
}

/// `true` when a handle envelope stamped `incoming_ms` replaces the stored
/// one. Strictly greater.
#[uniffi::export]
#[must_use]
pub fn conversation_handle_is_newer(incoming_ms: u64, stored_ms: u64) -> bool {
    rust_conversation_handle_is_newer(incoming_ms, stored_ms)
}

/// Shortest conversation-handle envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_handle_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MIN_LEN)
}

/// Longest conversation-handle envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_handle_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MAX_LEN)
}

// ============================================================================
// Handle certificate (lakko-attest, verify only)
// ============================================================================

/// The authenticated claims of a verified handle certificate.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHandleCertificate {
    /// The certified 64-byte lookup key of the name.
    pub lookup_key: Vec<u8>,
    /// The 32-byte handle key that holds the name: the key the
    /// conversation-handle signature and handle statements verify under.
    pub write_pk: Vec<u8>,
    /// When discovery issued it, UNIX ms. Informational.
    pub issued_at_unix_millis: i64,
}

impl From<HandleCertificate> for FfiHandleCertificate {
    fn from(c: HandleCertificate) -> Self {
        Self {
            lookup_key: c.lookup_key().to_vec(),
            write_pk: c.write_pk().to_vec(),
            issued_at_unix_millis: c.issued_at_unix_millis(),
        }
    }
}

fn handle_cert_error(e: &HandleCertError) -> FfiError {
    FfiError::UsernameCertificate(format!("{}: {e}", e.code()))
}

/// Verify a handle certificate under the pinned discovery key and its
/// binding to `expected_lookup_key` (the lookup key of the OPRF output the
/// viewer computed over the SHOWN name). Show the `@name` only on success.
///
/// # Errors
///
/// [`FfiError::UsernameCertificate`] with `badLength` / `badVersion` /
/// `badSignature` / `nameMismatch`, or `badVerifyingKey` / `lookupKeyLength`
/// for an argument.
#[uniffi::export]
pub fn verify_handle_binding(
    certificate: Vec<u8>,
    verifying_key: Vec<u8>,
    expected_lookup_key: Vec<u8>,
) -> Result<FfiHandleCertificate, FfiError> {
    let verifying = VerifyingKey::from_bytes(&verifying_key).map_err(|_| {
        FfiError::UsernameCertificate(
            "badVerifyingKey: the pinned discovery key is not an Ed25519 key".to_string(),
        )
    })?;
    let lookup = <[u8; 64]>::try_from(expected_lookup_key.as_slice()).map_err(|_| {
        FfiError::UsernameCertificate(format!(
            "lookupKeyLength: lookup key must be 64 bytes, got {}",
            expected_lookup_key.len()
        ))
    })?;
    rust_verify_handle_binding(&certificate, &verifying, &lookup)
        .map(Into::into)
        .map_err(|e| handle_cert_error(&e))
}

/// Wire length of a handle certificate: 169.
#[uniffi::export]
#[must_use]
pub fn handle_certificate_len() -> u32 {
    len_u32(HANDLE_CERT_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::FfiAvatarRef;
    use lakko_crypto::signing::SigningKey;

    fn avatar() -> FfiAvatar {
        FfiAvatar {
            still: FfiAvatarRef {
                blob_id: vec![1; 32],
                key: vec![2; 32],
                nonce: vec![3; 12],
                size: 1000,
                side: 320,
            },
            video: None,
            video_duration_ms: None,
        }
    }

    #[test]
    fn group_photo_round_trips_and_orders() {
        for avatar in [None, Some(avatar())] {
            let photo = FfiGroupPhoto {
                avatar,
                updated_at_ms: 7,
            };
            let wire = compose_group_photo_envelope(photo.clone()).unwrap();
            assert!(wire.len() >= group_photo_envelope_min_len() as usize);
            assert!(wire.len() <= group_photo_envelope_max_len() as usize);
            assert_eq!(decode_group_photo_envelope(wire).unwrap(), photo);
        }
        assert!(group_photo_is_newer(2, 1));
        assert!(!group_photo_is_newer(1, 1));
        let err = decode_group_photo_envelope(b"nope".to_vec()).unwrap_err();
        assert!(err.to_string().contains("groupPhoto.wrongDomain"), "{err}");
    }

    #[test]
    fn group_reactions_round_trip_and_refuse_a_bad_list() {
        for reactions in [
            FfiAllowedReactions::All,
            FfiAllowedReactions::Off,
            FfiAllowedReactions::Only {
                emoji: vec!["\u{1F44D}".into(), "\u{1F602}".into()],
            },
        ] {
            let value = FfiGroupReactions {
                reactions,
                updated_at_ms: 9,
            };
            let wire = compose_group_reactions_envelope(value.clone()).unwrap();
            assert!(wire.len() <= group_reactions_envelope_max_len() as usize);
            assert_eq!(decode_group_reactions_envelope(wire).unwrap(), value);
        }
        let bad = FfiGroupReactions {
            reactions: FfiAllowedReactions::Only { emoji: Vec::new() },
            updated_at_ms: 1,
        };
        assert!(compose_group_reactions_envelope(bad).is_err());
        assert!(validate_allowed_reactions(FfiAllowedReactions::Only {
            emoji: vec!["x".into(), "x".into()],
        })
        .is_err());
        assert!(allowed_reactions_allows(
            FfiAllowedReactions::All,
            "\u{1F44D}".into()
        ));
        assert!(!allowed_reactions_allows(
            FfiAllowedReactions::Off,
            "\u{1F44D}".into()
        ));
        assert!(group_reactions_is_newer(3, 2));
        assert_eq!(allowed_reactions_max(), 100);
    }

    #[test]
    fn conversation_handle_signs_composes_decodes_and_verifies() {
        let seed = vec![0x21_u8; 32];
        let handle_key = SigningKey::from_seed(&[0x21; 32])
            .verifying_key()
            .to_bytes();
        let group = vec![0x42_u8; 32];
        let signed = sign_conversation_handle(
            seed,
            group.clone(),
            5,
            true,
            "my_group".into(),
            vec![0; 169],
        )
        .unwrap();
        let wire = compose_conversation_handle_envelope(signed.clone()).unwrap();
        assert!(wire.len() >= conversation_handle_envelope_min_len() as usize);
        assert!(wire.len() <= conversation_handle_envelope_max_len() as usize);
        let decoded = decode_conversation_handle_envelope(wire).unwrap();
        assert_eq!(decoded, signed);
        verify_conversation_handle(decoded.clone(), group, handle_key.to_vec()).unwrap();
        assert!(verify_conversation_handle(decoded, vec![1; 32], handle_key.to_vec()).is_err());
        assert!(conversation_handle_is_newer(6, 5));
    }

    #[test]
    fn handle_certificate_verifies_its_binding() {
        let discovery = SigningKey::from_seed(&[0x31; 32]);
        let wire = HandleCertificate::new([0xAB; 64], [0xCD; 32], 11).sign(&discovery);
        let vk = discovery.verifying_key().to_bytes().to_vec();
        let got = verify_handle_binding(wire.to_vec(), vk.clone(), vec![0xAB; 64]).unwrap();
        assert_eq!(got.write_pk, vec![0xCD; 32]);
        assert_eq!(got.issued_at_unix_millis, 11);
        let err = verify_handle_binding(wire.to_vec(), vk, vec![0xAC; 64]).unwrap_err();
        assert!(err.to_string().contains("nameMismatch"), "{err}");
        assert_eq!(handle_certificate_len(), 169);
    }
}
