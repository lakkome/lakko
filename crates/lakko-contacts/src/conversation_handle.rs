// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The conversation-handle envelope: a group's `@name` and its public or
//! private flag, told to every member ([ADR-0087] section 11).
//!
//! ## Wire
//!
//! ```text
//! envelope = CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1 ("lakko/conversation-handle/v1\0")
//!            ‖ version(1)=0x01 ‖ updated_at_ms(8, BE) ‖ flag(1: 0 private, 1 public)
//!            ‖ handle_len(1: 3..=32) ‖ handle ‖ certificate(169) ‖ signature(64)
//! ```
//!
//! `handle` is a canonical `@name` ([`crate::username`]). `certificate` is
//! the discovery-signed handle certificate (`lakko_attest::handle_cert`),
//! carried opaquely here because this crate does not depend on
//! `lakko-attest`; its length is pinned against that crate's where both are
//! visible. `signature` is Ed25519 by the handle key over
//!
//! ```text
//! CONVERSATION_HANDLE_SIGNATURE_DOMAIN ‖ group_id_len(1) ‖ group_id
//!   ‖ (the envelope from the version byte through the certificate)
//! ```
//!
//! so an envelope signed for one group cannot be replayed into another.
//!
//! ## Why any member may relay it
//!
//! The certificate binds the name to the handle key, and the signature binds
//! the flag, the stamp and this group to the same key. A receiver checks
//! both (the certificate with one OPRF evaluation over the shown name, the
//! signature under the certificate's key, [`ConversationHandle::verify_signature`])
//! and needs no role check: a newcomer gets the envelope at the growth
//! re-send whether or not the owner is online. Strictly newer stamp wins
//! ([`conversation_handle_is_newer`]), and a re-send carries the original
//! stamp verbatim (the ADR-0044 lesson).
//!
//! ## Bounds
//!
//! Exact length (one `handle_len` gives one envelope length), a known version,
//! a flag of 0 or 1 and a canonical handle; anything else refuses the whole
//! envelope. Dispatched as [`crate::incoming_envelope::IncomingEnvelope::ConversationHandle`],
//! never rendered.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

use crate::username::{
    check_canonical_username, UsernameError, USERNAME_MAX_LEN, USERNAME_MIN_LEN,
};

/// Domain separator that opens the envelope. Frozen: the dispatcher matches
/// on it.
pub const CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/conversation-handle/v1\x00";

/// Domain separator of the handle key's signature.
pub const CONVERSATION_HANDLE_SIGNATURE_DOMAIN: &[u8] = b"lakko/conversation-handle/sig/v1\x00";

/// Envelope version.
pub const CONVERSATION_HANDLE_VERSION_V1: u8 = 0x01;

/// Length of the handle certificate the envelope carries
/// (`lakko_attest::HANDLE_CERT_LEN`).
pub const CONVERSATION_HANDLE_CERT_LEN: usize = 169;

/// Longest group id the signature binds.
pub const CONVERSATION_HANDLE_MAX_GROUP_ID_LEN: usize = u8::MAX as usize;

const FIXED_LEN: usize = 1 + 8 + 1 + 1 + CONVERSATION_HANDLE_CERT_LEN + SIGNATURE_LEN;

/// Shortest envelope (a three-character handle).
pub const CONVERSATION_HANDLE_ENVELOPE_MIN_LEN: usize =
    CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1.len() + FIXED_LEN + USERNAME_MIN_LEN;

/// Longest envelope (a 32-character handle).
pub const CONVERSATION_HANDLE_ENVELOPE_MAX_LEN: usize =
    CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1.len() + FIXED_LEN + USERNAME_MAX_LEN;

/// A group's `@name`, its flag and the proof the owner's handle key signed
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHandle {
    /// Owner-clock Unix milliseconds of the change. Ordered strictly.
    pub updated_at_ms: u64,
    /// `true` for a public group.
    pub public: bool,
    /// The canonical `@name`, without `@`.
    pub handle: String,
    /// The discovery handle certificate, opaque here.
    pub certificate: [u8; CONVERSATION_HANDLE_CERT_LEN],
    /// Ed25519 signature by the handle key (see the module docs).
    pub signature: [u8; SIGNATURE_LEN],
}

/// Why a conversation-handle envelope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConversationHandleError {
    /// The domain did not match (a dispatcher fall-through, never surfaced
    /// by [`try_unwrap_conversation_handle`]).
    #[error("not a conversation-handle envelope")]
    WrongDomain,
    /// An unknown version byte.
    #[error("unknown conversation-handle version {0}")]
    UnknownVersion(u8),
    /// The length does not match the handle length, or a field is cut short.
    #[error("conversation-handle envelope has the wrong length")]
    BadLength,
    /// The flag byte is neither 0 nor 1.
    #[error("conversation-handle flag is not 0 or 1")]
    BadFlag,
    /// The handle is not a canonical `@name`.
    #[error("conversation-handle name: {0}")]
    Handle(UsernameError),
    /// The group id is longer than [`CONVERSATION_HANDLE_MAX_GROUP_ID_LEN`].
    #[error("group id too long")]
    GroupIdTooLong,
    /// The signature does not verify under the given handle key.
    #[error("conversation-handle signature does not verify")]
    BadSignature,
}

impl ConversationHandleError {
    /// Stable, language-neutral reason code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadLength => "badLength",
            Self::BadFlag => "badFlag",
            Self::Handle(_) => "badHandle",
            Self::GroupIdTooLong => "groupIdTooLong",
            Self::BadSignature => "badSignature",
        }
    }
}

impl ConversationHandle {
    /// Build and sign an envelope with the handle seed for `group_id`.
    ///
    /// # Errors
    ///
    /// [`ConversationHandleError::Handle`] for a non-canonical handle,
    /// [`ConversationHandleError::GroupIdTooLong`].
    pub fn sign(
        handle_seed: &[u8; 32],
        group_id: &[u8],
        updated_at_ms: u64,
        public: bool,
        handle: &str,
        certificate: [u8; CONVERSATION_HANDLE_CERT_LEN],
    ) -> CoreResult<Self, ConversationHandleError> {
        check_canonical_username(handle).map_err(ConversationHandleError::Handle)?;
        let mut envelope = Self {
            updated_at_ms,
            public,
            handle: handle.to_string(),
            certificate,
            signature: [0u8; SIGNATURE_LEN],
        };
        let message = envelope.signed_message(group_id)?;
        envelope.signature = SigningKey::from_seed(handle_seed).sign(&message).to_bytes();
        Ok(envelope)
    }

    /// The bytes from the version through the certificate.
    fn body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIXED_LEN + self.handle.len());
        out.push(CONVERSATION_HANDLE_VERSION_V1);
        out.extend_from_slice(&self.updated_at_ms.to_be_bytes());
        out.push(u8::from(self.public));
        // The handle is canonical, so 3..=32 bytes: fits one byte.
        out.push(u8::try_from(self.handle.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(self.handle.as_bytes());
        out.extend_from_slice(&self.certificate);
        out
    }

    fn signed_message(&self, group_id: &[u8]) -> CoreResult<Vec<u8>, ConversationHandleError> {
        let group_id_len =
            u8::try_from(group_id.len()).map_err(|_| ConversationHandleError::GroupIdTooLong)?;
        let mut out = Vec::with_capacity(
            CONVERSATION_HANDLE_SIGNATURE_DOMAIN.len() + 1 + group_id.len() + FIXED_LEN,
        );
        out.extend_from_slice(CONVERSATION_HANDLE_SIGNATURE_DOMAIN);
        out.push(group_id_len);
        out.extend_from_slice(group_id);
        out.extend_from_slice(&self.body());
        Ok(out)
    }

    /// Verify the signature for `group_id` under `handle_key`, the key the
    /// verified certificate names.
    ///
    /// # Errors
    ///
    /// [`ConversationHandleError::BadSignature`],
    /// [`ConversationHandleError::GroupIdTooLong`].
    pub fn verify_signature(
        &self,
        group_id: &[u8],
        handle_key: &[u8; 32],
    ) -> CoreResult<(), ConversationHandleError> {
        let key = VerifyingKey::from_bytes(handle_key)
            .map_err(|_| ConversationHandleError::BadSignature)?;
        let signature = Signature::from_bytes(&self.signature)
            .map_err(|_| ConversationHandleError::BadSignature)?;
        key.verify(&self.signed_message(group_id)?, &signature)
            .map_err(|_| ConversationHandleError::BadSignature)
    }
}

/// Encode an envelope.
///
/// # Errors
///
/// [`ConversationHandleError::Handle`] when the handle is not canonical, so
/// nothing is composed that a receiver would refuse.
pub fn compose_conversation_handle(
    envelope: &ConversationHandle,
) -> CoreResult<Vec<u8>, ConversationHandleError> {
    check_canonical_username(&envelope.handle).map_err(ConversationHandleError::Handle)?;
    let mut out = Vec::with_capacity(CONVERSATION_HANDLE_ENVELOPE_MAX_LEN);
    out.extend_from_slice(CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1);
    out.extend_from_slice(&envelope.body());
    out.extend_from_slice(&envelope.signature);
    Ok(out)
}

/// Discriminated unwrap for the dispatcher: `Ok(None)` when the domain does
/// not match.
///
/// # Errors
///
/// Every [`ConversationHandleError`] except `WrongDomain`,
/// `GroupIdTooLong` and `BadSignature`.
pub fn try_unwrap_conversation_handle(
    plaintext: &[u8],
) -> CoreResult<Option<ConversationHandle>, ConversationHandleError> {
    let Some(payload) = plaintext.strip_prefix(CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(ConversationHandleError::BadLength)?;
    if version != CONVERSATION_HANDLE_VERSION_V1 {
        return Err(ConversationHandleError::UnknownVersion(version));
    }
    if rest.len() < 8 + 1 + 1 {
        return Err(ConversationHandleError::BadLength);
    }
    let (stamp, rest) = rest.split_at(8);
    let (&flag, rest) = rest
        .split_first()
        .ok_or(ConversationHandleError::BadLength)?;
    let (&handle_len, rest) = rest
        .split_first()
        .ok_or(ConversationHandleError::BadLength)?;
    let handle_len = usize::from(handle_len);
    if rest.len() != handle_len + CONVERSATION_HANDLE_CERT_LEN + SIGNATURE_LEN {
        return Err(ConversationHandleError::BadLength);
    }
    let public = match flag {
        0 => false,
        1 => true,
        _ => return Err(ConversationHandleError::BadFlag),
    };
    let (handle_bytes, rest) = rest.split_at(handle_len);
    let handle = core::str::from_utf8(handle_bytes)
        .map_err(|_| ConversationHandleError::Handle(UsernameError::InvalidCharacters))?;
    check_canonical_username(handle).map_err(ConversationHandleError::Handle)?;
    let (certificate, signature) = rest.split_at(CONVERSATION_HANDLE_CERT_LEN);
    let to_len = |_| ConversationHandleError::BadLength;
    Ok(Some(ConversationHandle {
        updated_at_ms: u64::from_be_bytes(stamp.try_into().map_err(to_len)?),
        public,
        handle: handle.to_string(),
        certificate: certificate.try_into().map_err(to_len)?,
        signature: signature.try_into().map_err(to_len)?,
    }))
}

/// Decode an envelope that must be one.
///
/// # Errors
///
/// [`ConversationHandleError::WrongDomain`] and every error of
/// [`try_unwrap_conversation_handle`].
pub fn decode_conversation_handle(
    bytes: &[u8],
) -> CoreResult<ConversationHandle, ConversationHandleError> {
    try_unwrap_conversation_handle(bytes)?.ok_or(ConversationHandleError::WrongDomain)
}

/// Whether an incoming stamp replaces a stored one: strictly greater only,
/// so a re-send of the stored envelope is not an update.
#[must_use]
pub const fn conversation_handle_is_newer(incoming_ms: u64, stored_ms: u64) -> bool {
    incoming_ms > stored_ms
}

/// The fuzz oracle behind `fuzz_api::conversation_handle_decode`: whatever
/// decodes re-composes to the same bytes, lies within the advertised
/// lengths, and verifies (or fails to) without a panic. `None` when the
/// decoder refuses.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let envelope = decode_conversation_handle(bytes).ok()?;
    let _ = envelope.verify_signature(&[0x47; 32], &[0x48; 32]);
    let recomposed = compose_conversation_handle(&envelope).ok()? == bytes;
    let within = (CONVERSATION_HANDLE_ENVELOPE_MIN_LEN..=CONVERSATION_HANDLE_ENVELOPE_MAX_LEN)
        .contains(&bytes.len());
    Some(
        recomposed
            && within
            && !conversation_handle_is_newer(envelope.updated_at_ms, envelope.updated_at_ms),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SEED: [u8; 32] = [0x51; 32];
    const GROUP: [u8; 32] = [0x52; 32];

    fn sample() -> ConversationHandle {
        ConversationHandle::sign(
            &SEED,
            &GROUP,
            1_800_000_000_000,
            true,
            "hiking_club",
            [7; 169],
        )
        .unwrap()
    }

    fn key() -> [u8; 32] {
        SigningKey::from_seed(&SEED).verifying_key().to_bytes()
    }

    #[test]
    fn round_trips_and_verifies_for_its_group_only() {
        let env = sample();
        let wire = compose_conversation_handle(&env).unwrap();
        assert_eq!(wire.len(), CONVERSATION_HANDLE_ENVELOPE_MIN_LEN + 8);
        let back = decode_conversation_handle(&wire).unwrap();
        assert_eq!(back, env);
        back.verify_signature(&GROUP, &key()).expect("verifies");
        assert_eq!(
            back.verify_signature(&[0x53; 32], &key()),
            Err(ConversationHandleError::BadSignature)
        );
        assert_eq!(
            back.verify_signature(
                &GROUP,
                &SigningKey::from_seed(&[0x54; 32])
                    .verifying_key()
                    .to_bytes()
            ),
            Err(ConversationHandleError::BadSignature)
        );
    }

    #[test]
    fn every_signed_field_is_bound() {
        let wire = compose_conversation_handle(&sample()).unwrap();
        let d = CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1.len();
        // stamp, flag, a handle byte, a certificate byte
        for at in [d + 1, d + 9, d + 11, d + 11 + 11 + 5] {
            let mut bad = wire.clone();
            bad[at] ^= if at == d + 9 { 1 } else { 0x02 };
            // A change that makes it malformed is a refusal too.
            if let Ok(env) = decode_conversation_handle(&bad) {
                assert_eq!(
                    env.verify_signature(&GROUP, &key()),
                    Err(ConversationHandleError::BadSignature),
                    "byte {at}"
                );
            }
        }
    }

    #[test]
    fn malformed_envelopes_are_refused() {
        let wire = compose_conversation_handle(&sample()).unwrap();
        let d = CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            decode_conversation_handle(&wire[..wire.len() - 1]),
            Err(ConversationHandleError::BadLength)
        );
        let mut v = wire.clone();
        v[d] = 2;
        assert_eq!(
            decode_conversation_handle(&v),
            Err(ConversationHandleError::UnknownVersion(2))
        );
        let mut f = wire.clone();
        f[d + 9] = 2;
        assert_eq!(
            decode_conversation_handle(&f),
            Err(ConversationHandleError::BadFlag)
        );
        let mut h = wire.clone();
        h[d + 11] = b'-';
        assert!(matches!(
            decode_conversation_handle(&h),
            Err(ConversationHandleError::Handle(_))
        ));
        assert_eq!(
            try_unwrap_conversation_handle(b"lakko/other/v1\0x"),
            Ok(None)
        );
        assert!(matches!(
            ConversationHandle::sign(&SEED, &GROUP, 1, false, "ab", [0; 169]),
            Err(ConversationHandleError::Handle(_))
        ));
    }

    #[test]
    fn the_order_is_strict() {
        assert!(conversation_handle_is_newer(2, 1));
        assert!(!conversation_handle_is_newer(1, 1));
        assert!(!conversation_handle_is_newer(0, 1));
    }

    proptest! {
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = decode_conversation_handle(&bytes);
        }

        #[test]
        fn decode_of_a_prefixed_payload_never_panics(
            tail in proptest::collection::vec(any::<u8>(), 0..300)
        ) {
            let mut bytes = CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Ok(env) = decode_conversation_handle(&bytes) {
                prop_assert_eq!(compose_conversation_handle(&env).unwrap(), bytes);
            }
        }
    }
}
