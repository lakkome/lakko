// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Attested chat messages ([ADR-0089] section 1).
//!
//! A chat envelope travels inside an MLS application message wrapped with a
//! signature by the sending device's conversation leaf key, so a report can
//! hand a message to a moderator who can check that the reported account
//! wrote it. OpenMLS 0.8.1 keeps its own message signature crate-private and
//! signs the epoch's group context with it, so it cannot serve as evidence.
//!
//! ```text
//! wire          = DOMAIN ‖ version(1) ‖ sig(64) ‖ envelope
//! signing input = SIG_DOMAIN ‖ version ‖ group_id_len(1) ‖ group_id ‖ BLAKE3(envelope)
//! ```
//!
//! `envelope` is the JSON chat envelope byte for byte. `group_id` binds the
//! message to its conversation: a signature made in one conversation does
//! not verify as one made in another.
//!
//! What the signature does not say: when the message was sent, that it was
//! delivered, or in what order. The envelope carries no trusted time.
//!
//! A receiver checks the signature against the MLS sender leaf it resolved
//! at receipt and records the result. A missing or failed signature drops
//! nothing: it decides only whether the message can serve as evidence.
//!
//! [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md

use lakko_crypto::hash::blake3;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

use crate::{parse_body, MessageBody, SERVICE_ENVELOPE_PREFIX};

/// The domain separator an attested message opens with. Frozen.
pub const ATTESTED_DOMAIN: &[u8] = b"lakko/attested-message/v1\0";

/// The only wire version.
pub const ATTESTED_VERSION: u8 = 0x01;

/// The domain separator of the signing input. Frozen; distinct from
/// [`ATTESTED_DOMAIN`] so the wire prefix can never be read as a signing input.
pub const ATTESTED_SIG_DOMAIN: &[u8] = b"lakko/attested-message/sig/v1\0";

/// Bytes the wrapper adds before the envelope.
pub const ATTESTED_OVERHEAD: usize = ATTESTED_DOMAIN.len() + 1 + SIGNATURE_LEN;

/// The longest group id a signature binds (a one-byte length prefix).
pub const MAX_GROUP_ID_LEN: usize = 255;

/// Ed25519 public key length.
const KEY_LEN: usize = 32;

/// What a receiver learned about a message's signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attestation {
    /// The signature verified under the sender's leaf key.
    Verified,
    /// The message carried the wrapper but its signature did not verify, or
    /// the wrapper was malformed.
    Failed,
    /// The message carried no wrapper.
    Absent,
}

/// Why a message could not be attested or verified.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttestError {
    /// The group id is empty or longer than [`MAX_GROUP_ID_LEN`].
    #[error("group id length {0} is outside 1..=255")]
    BadGroupId(usize),
    /// The bytes to sign are a service envelope or already attested; only a
    /// chat envelope is attested.
    #[error("only a chat envelope can be attested")]
    NotAChatEnvelope,
    /// The signing seed does not match the expected leaf key.
    #[error("signing key does not match the leaf key")]
    KeyMismatch,
    /// The wire does not open with [`ATTESTED_DOMAIN`].
    #[error("not an attested message")]
    NotAttested,
    /// The wrapper is too short or names another version.
    #[error("malformed attested message")]
    Malformed,
    /// The signer key is not a valid Ed25519 public key.
    #[error("bad signer key")]
    BadKey,
    /// The signature does not verify.
    #[error("signature does not verify")]
    BadSignature,
}

impl AttestError {
    /// A stable code for the FFI surfaces.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadGroupId(_) => "bad-group-id",
            Self::NotAChatEnvelope => "not-a-chat-envelope",
            Self::KeyMismatch => "key-mismatch",
            Self::NotAttested => "not-attested",
            Self::Malformed => "malformed",
            Self::BadKey => "bad-key",
            Self::BadSignature => "bad-signature",
        }
    }
}

/// The bytes the leaf key signs.
///
/// # Errors
///
/// [`AttestError::BadGroupId`].
pub fn signing_input(group_id: &[u8], envelope: &[u8]) -> Result<Vec<u8>, AttestError> {
    let gid_len = u8::try_from(group_id.len())
        .ok()
        .filter(|l| *l > 0)
        .ok_or(AttestError::BadGroupId(group_id.len()))?;
    let mut out = Vec::with_capacity(ATTESTED_SIG_DOMAIN.len() + 2 + group_id.len() + 32);
    out.extend_from_slice(ATTESTED_SIG_DOMAIN);
    out.push(ATTESTED_VERSION);
    out.push(gid_len);
    out.extend_from_slice(group_id);
    out.extend_from_slice(&blake3(envelope));
    Ok(out)
}

/// Whether `bytes` opens with the attested-message domain.
#[must_use]
pub fn is_attested(bytes: &[u8]) -> bool {
    bytes.starts_with(ATTESTED_DOMAIN)
}

/// Wrap a chat envelope with a signature by the leaf whose Ed25519 seed is
/// `leaf_seed`. When `expected_leaf_key` is given, the seed must derive it,
/// so a caller cannot sign with a key that is not its leaf.
///
/// # Errors
///
/// [`AttestError::BadGroupId`], [`AttestError::NotAChatEnvelope`] (the
/// envelope opens with `lakko/`: a service envelope or an attested message),
/// [`AttestError::KeyMismatch`].
pub fn attest(
    leaf_seed: &[u8; 32],
    expected_leaf_key: Option<&[u8]>,
    group_id: &[u8],
    envelope: &[u8],
) -> Result<Vec<u8>, AttestError> {
    if envelope.starts_with(SERVICE_ENVELOPE_PREFIX) {
        return Err(AttestError::NotAChatEnvelope);
    }
    let input = signing_input(group_id, envelope)?;
    let key = SigningKey::from_seed(leaf_seed);
    if let Some(expected) = expected_leaf_key {
        if key.verifying_key().to_bytes().as_slice() != expected {
            return Err(AttestError::KeyMismatch);
        }
    }
    let sig = key.sign(&input).to_bytes();
    let mut out = Vec::with_capacity(ATTESTED_OVERHEAD + envelope.len());
    out.extend_from_slice(ATTESTED_DOMAIN);
    out.push(ATTESTED_VERSION);
    out.extend_from_slice(&sig);
    out.extend_from_slice(envelope);
    Ok(out)
}

/// Split an attested wire into its signature and envelope without verifying.
///
/// # Errors
///
/// [`AttestError::NotAttested`], [`AttestError::Malformed`].
pub fn split(wire: &[u8]) -> Result<([u8; SIGNATURE_LEN], &[u8]), AttestError> {
    let rest = wire
        .strip_prefix(ATTESTED_DOMAIN)
        .ok_or(AttestError::NotAttested)?;
    let (&version, rest) = rest.split_first().ok_or(AttestError::Malformed)?;
    if version != ATTESTED_VERSION || rest.len() < SIGNATURE_LEN {
        return Err(AttestError::Malformed);
    }
    let (sig, envelope) = rest.split_at(SIGNATURE_LEN);
    let mut out = [0u8; SIGNATURE_LEN];
    out.copy_from_slice(sig);
    Ok((out, envelope))
}

/// Verify an attested wire made in `group_id` by the leaf `signer_key`, and
/// return the envelope.
///
/// # Errors
///
/// Every [`AttestError`] but [`AttestError::NotAChatEnvelope`] and
/// [`AttestError::KeyMismatch`].
pub fn verify<'a>(
    wire: &'a [u8],
    group_id: &[u8],
    signer_key: &[u8],
) -> Result<&'a [u8], AttestError> {
    let (sig, envelope) = split(wire)?;
    if signer_key.len() != KEY_LEN {
        return Err(AttestError::BadKey);
    }
    let key = VerifyingKey::from_bytes(signer_key).map_err(|_| AttestError::BadKey)?;
    let sig = Signature::from_bytes(&sig).map_err(|_| AttestError::BadSignature)?;
    let input = signing_input(group_id, envelope)?;
    key.verify(&input, &sig)
        .map_err(|_| AttestError::BadSignature)?;
    Ok(envelope)
}

/// The receive path: classify a decrypted application plaintext and say what
/// its signature showed.
///
/// - No wrapper: [`parse_body`] of the bytes, [`Attestation::Absent`].
/// - A malformed wrapper: [`MessageBody::ServiceEnvelope`] (dropped, there is
///   no envelope to show), [`Attestation::Failed`].
/// - A well-formed wrapper: [`parse_body`] of the envelope, and
///   [`Attestation::Verified`] or [`Attestation::Failed`].
#[must_use]
pub fn parse_attested_body(
    plaintext: &[u8],
    group_id: &[u8],
    signer_key: &[u8],
) -> (MessageBody, Attestation) {
    if !is_attested(plaintext) {
        return (parse_body(plaintext), Attestation::Absent);
    }
    match split(plaintext) {
        Err(_) => (MessageBody::ServiceEnvelope, Attestation::Failed),
        Ok((_, envelope)) => {
            let attestation = if verify(plaintext, group_id, signer_key).is_ok() {
                Attestation::Verified
            } else {
                Attestation::Failed
            };
            (parse_body(envelope), attestation)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SEED: [u8; 32] = [7u8; 32];

    fn leaf_key(seed: &[u8; 32]) -> Vec<u8> {
        SigningKey::from_seed(seed)
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    fn chat(text: &str) -> Vec<u8> {
        crate::compose(text, Vec::new()).unwrap()
    }

    #[test]
    fn a_signed_message_verifies_and_parses() {
        let env = chat("hello");
        let wire = attest(&SEED, Some(&leaf_key(&SEED)), b"group-a", &env).unwrap();
        assert_eq!(wire.len(), ATTESTED_OVERHEAD + env.len());
        assert_eq!(
            verify(&wire, b"group-a", &leaf_key(&SEED)).unwrap(),
            env.as_slice()
        );
        let (body, att) = parse_attested_body(&wire, b"group-a", &leaf_key(&SEED));
        assert_eq!(att, Attestation::Verified);
        assert!(matches!(body, MessageBody::Chat { ref text, .. } if text == "hello"));
    }

    #[test]
    fn another_conversation_does_not_verify() {
        let wire = attest(&SEED, None, b"group-a", &chat("x")).unwrap();
        assert_eq!(
            verify(&wire, b"group-b", &leaf_key(&SEED)),
            Err(AttestError::BadSignature)
        );
        let (body, att) = parse_attested_body(&wire, b"group-b", &leaf_key(&SEED));
        assert_eq!(att, Attestation::Failed);
        assert!(matches!(body, MessageBody::Chat { .. }));
    }

    #[test]
    fn another_signer_does_not_verify() {
        let wire = attest(&SEED, None, b"g", &chat("x")).unwrap();
        assert_eq!(
            verify(&wire, b"g", &leaf_key(&[8u8; 32])),
            Err(AttestError::BadSignature)
        );
    }

    #[test]
    fn an_unwrapped_message_is_absent() {
        let env = chat("plain");
        let (body, att) = parse_attested_body(&env, b"g", &leaf_key(&SEED));
        assert_eq!(att, Attestation::Absent);
        assert!(matches!(body, MessageBody::Chat { ref text, .. } if text == "plain"));
    }

    #[test]
    fn a_malformed_wrapper_is_dropped() {
        let mut wire = ATTESTED_DOMAIN.to_vec();
        wire.push(ATTESTED_VERSION);
        wire.extend_from_slice(&[0u8; 10]);
        let (body, att) = parse_attested_body(&wire, b"g", &leaf_key(&SEED));
        assert_eq!(att, Attestation::Failed);
        assert_eq!(body, MessageBody::ServiceEnvelope);
        let mut v2 = ATTESTED_DOMAIN.to_vec();
        v2.push(2);
        v2.extend_from_slice(&[0u8; 80]);
        assert_eq!(split(&v2), Err(AttestError::Malformed));
    }

    #[test]
    fn service_envelopes_and_wrappers_are_not_attested() {
        assert_eq!(
            attest(&SEED, None, b"g", b"lakko/profile/v1\0xx"),
            Err(AttestError::NotAChatEnvelope)
        );
        let wire = attest(&SEED, None, b"g", &chat("x")).unwrap();
        assert_eq!(
            attest(&SEED, None, b"g", &wire),
            Err(AttestError::NotAChatEnvelope)
        );
    }

    #[test]
    fn the_seed_must_be_the_leaf() {
        assert_eq!(
            attest(&SEED, Some(&leaf_key(&[9u8; 32])), b"g", &chat("x")),
            Err(AttestError::KeyMismatch)
        );
    }

    #[test]
    fn group_id_bounds() {
        assert_eq!(
            attest(&SEED, None, b"", &chat("x")),
            Err(AttestError::BadGroupId(0))
        );
        assert_eq!(
            attest(&SEED, None, &[0u8; 256], &chat("x")),
            Err(AttestError::BadGroupId(256))
        );
        assert!(attest(&SEED, None, &[0u8; 255], &chat("x")).is_ok());
    }

    #[test]
    fn the_wire_prefix_is_a_service_envelope_to_an_unaware_parser() {
        let wire = attest(&SEED, None, b"g", &chat("x")).unwrap();
        assert_eq!(parse_body(&wire), MessageBody::ServiceEnvelope);
    }

    proptest! {
        #[test]
        fn any_flipped_byte_fails(text in ".{0,40}", gid in proptest::collection::vec(any::<u8>(), 1..64), pos in any::<usize>(), bit in 0u8..8) {
            let env = chat(&text);
            let wire = attest(&SEED, None, &gid, &env).unwrap();
            let i = ATTESTED_DOMAIN.len() + 1 + pos % (wire.len() - ATTESTED_DOMAIN.len() - 1);
            let mut bad = wire.clone();
            bad[i] ^= 1 << bit;
            prop_assert!(verify(&bad, &gid, &leaf_key(&SEED)).is_err());
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = parse_attested_body(&bytes, b"g", &leaf_key(&SEED));
            let mut prefixed = ATTESTED_DOMAIN.to_vec();
            prefixed.extend_from_slice(&bytes);
            let (_, att) = parse_attested_body(&prefixed, b"g", &leaf_key(&SEED));
            prop_assert_ne!(att, Attestation::Verified);
        }
    }
}
