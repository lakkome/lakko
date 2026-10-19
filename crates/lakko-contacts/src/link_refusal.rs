// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Link refusal — the inviter's answer to a contact request whose invite link
//! no longer counts
//! ([ADR-0079](../../../docs/adr/0079-invite-link-makes-contacts.md),
//! amendment of 2026-10-19; `docs/ux/contact-invite-link.md` sections 4-5).
//!
//! ## Why this exists
//!
//! B pressed **Add** on A's invite page and holds A as a pending contact. When
//! A's app takes the request and finds that the link no longer counts (revoked
//! before A's app answered, expired before B pressed Add, used up, or a wrong
//! secret), A's app drops the request silently and sends this refusal back the
//! way the request said to answer (its `reply` grant, B's account mailbox). B's
//! app, on a refusal that verifies, removes the pending contact and the
//! messages held for it, silently.
//!
//! ## Wire format (v1)
//!
//! ```text
//! LINK_REFUSAL_DOMAIN_V1 || version(1) || link_id(16) || requester_account_key(32) || sig(64)
//! ```
//!
//! Fixed length: [`LINK_REFUSAL_LEN`] bytes. `sig` is Ed25519 by the inviter's
//! account key over every byte before it. It travels as the body of a sealed
//! mailbox row of kind `LinkRefusal` (ADR-0078), so the Delivery Service sees
//! an opaque row.
//!
//! ## Who can cancel B's pending contact
//!
//! Only the holder of A's account key: B verifies the signature against the
//! account key the invite link named (kept with the pending contact), and
//! requires the link id to be the link B answered and the requester key to be
//! B's own account key ([`verify_link_refusal`]). A refusal minted for another
//! person, another link or by anyone but A is refused, so nobody else can
//! remove B's pending contact and A cannot replay one refusal against a
//! different request.
//!
//! The refusal carries no time: it is idempotent (removing a pending contact
//! twice changes nothing), and B ignores it once the one-to-one with A exists.

use core::result::Result as CoreResult;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

use crate::invite::LINK_ID_LEN;

/// 22-byte domain separator. The trailing `\x00` keeps a UTF-8 payload from
/// ever starting with it. Frozen; the version byte is the knob that moves.
pub const LINK_REFUSAL_DOMAIN_V1: &[u8] = b"lakko/link-refusal/v1\x00";

/// Schema-version byte carried first after the separator.
pub const LINK_REFUSAL_VERSION_V1: u8 = 0x01;

/// Length of the requester's account key (Ed25519 public key).
pub const LINK_REFUSAL_REQUESTER_KEY_LEN: usize = 32;

/// Bytes after the separator: version, link id, requester key, signature.
const PAYLOAD_LEN: usize = 1 + LINK_ID_LEN + LINK_REFUSAL_REQUESTER_KEY_LEN + SIGNATURE_LEN;

/// The one well-formed length.
pub const LINK_REFUSAL_LEN: usize = LINK_REFUSAL_DOMAIN_V1.len() + PAYLOAD_LEN;

/// The bytes a refusal signs: everything before the signature.
const SIGNED_LEN: usize = LINK_REFUSAL_LEN - SIGNATURE_LEN;

/// A decoded refusal. Decoding checks the layout only; the signature is
/// checked by [`verify_link_refusal`] against a key the caller already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRefusal {
    /// The id of the invite link the refused request answered.
    pub link_id: [u8; LINK_ID_LEN],
    /// The requester's account key: the person the refusal is for.
    pub requester_account_key: [u8; LINK_REFUSAL_REQUESTER_KEY_LEN],
    /// Ed25519 signature by the inviter's account key.
    pub signature: [u8; SIGNATURE_LEN],
}

fn signed_bytes(
    link_id: &[u8; LINK_ID_LEN],
    requester_account_key: &[u8; LINK_REFUSAL_REQUESTER_KEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(LINK_REFUSAL_LEN);
    out.extend_from_slice(LINK_REFUSAL_DOMAIN_V1);
    out.push(LINK_REFUSAL_VERSION_V1);
    out.extend_from_slice(link_id);
    out.extend_from_slice(requester_account_key);
    out
}

/// Compose a refusal signed by the inviter's account key. Infallible: every
/// input is bounded by its type, and [`decode_link_refusal`] accepts exactly
/// what this produces.
#[must_use]
pub fn compose_link_refusal(
    inviter: &SigningKey,
    link_id: &[u8; LINK_ID_LEN],
    requester_account_key: &[u8; LINK_REFUSAL_REQUESTER_KEY_LEN],
) -> Vec<u8> {
    let mut out = signed_bytes(link_id, requester_account_key);
    let sig = inviter.sign(&out);
    out.extend_from_slice(&sig.to_bytes());
    out
}

/// Decode a refusal's layout (no signature check). Fail-closed: the domain,
/// then the version (BEFORE the length, so a later revision that changes the
/// length reports as a version), then the exact length.
///
/// # Errors
///
/// [`LinkRefusalError::WrongDomain`], [`LinkRefusalError::UnknownVersion`],
/// [`LinkRefusalError::BadLength`].
pub fn decode_link_refusal(bytes: &[u8]) -> CoreResult<LinkRefusal, LinkRefusalError> {
    let payload = bytes
        .strip_prefix(LINK_REFUSAL_DOMAIN_V1)
        .ok_or(LinkRefusalError::WrongDomain)?;
    let bad = || LinkRefusalError::BadLength {
        actual: payload.len(),
    };
    let (&version, rest) = payload.split_first().ok_or_else(bad)?;
    if version != LINK_REFUSAL_VERSION_V1 {
        return Err(LinkRefusalError::UnknownVersion(version));
    }
    if rest.len() != PAYLOAD_LEN - 1 {
        return Err(bad());
    }
    let (link_id, rest) = rest.split_at(LINK_ID_LEN);
    let (requester, sig) = rest.split_at(LINK_REFUSAL_REQUESTER_KEY_LEN);
    Ok(LinkRefusal {
        link_id: link_id.try_into().map_err(|_| bad())?,
        requester_account_key: requester.try_into().map_err(|_| bad())?,
        signature: sig.try_into().map_err(|_| bad())?,
    })
}

/// Verify a refusal B received: the layout, that it names `expected_link_id`
/// (the link B answered) and `own_account_key` (B), and that it is signed by
/// `inviter_key` (the account key the invite link named, kept with B's pending
/// contact). Every failure is a refusal of the refusal; the caller then
/// changes nothing.
///
/// # Errors
///
/// Every [`decode_link_refusal`] error, [`LinkRefusalError::OtherLink`],
/// [`LinkRefusalError::OtherRequester`], [`LinkRefusalError::BadKey`],
/// [`LinkRefusalError::BadSignature`].
pub fn verify_link_refusal(
    bytes: &[u8],
    inviter_key: &[u8],
    expected_link_id: &[u8; LINK_ID_LEN],
    own_account_key: &[u8; LINK_REFUSAL_REQUESTER_KEY_LEN],
) -> CoreResult<LinkRefusal, LinkRefusalError> {
    let refusal = decode_link_refusal(bytes)?;
    if &refusal.link_id != expected_link_id {
        return Err(LinkRefusalError::OtherLink);
    }
    if &refusal.requester_account_key != own_account_key {
        return Err(LinkRefusalError::OtherRequester);
    }
    let key = VerifyingKey::from_bytes(inviter_key).map_err(|_| LinkRefusalError::BadKey)?;
    let sig =
        Signature::from_bytes(&refusal.signature).map_err(|_| LinkRefusalError::BadSignature)?;
    let signed = bytes.get(..SIGNED_LEN).ok_or(LinkRefusalError::BadLength {
        actual: bytes.len(),
    })?;
    key.verify(signed, &sig)
        .map_err(|_| LinkRefusalError::BadSignature)?;
    Ok(refusal)
}

/// Why a refusal was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkRefusalError {
    /// The bytes do not carry the refusal domain separator.
    #[error("bytes do not carry the link-refusal domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("link refusal has {actual} payload bytes, expected {PAYLOAD_LEN}")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The version byte is not [`LINK_REFUSAL_VERSION_V1`].
    #[error("link refusal carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The refusal names another link than the one the pending contact
    /// answered.
    #[error("link refusal names another link")]
    OtherLink,
    /// The refusal names another requester than this account.
    #[error("link refusal names another requester")]
    OtherRequester,
    /// The inviter key the caller holds is not a valid Ed25519 key.
    #[error("inviter key is not a valid Ed25519 key")]
    BadKey,
    /// The signature does not verify under the inviter's key.
    #[error("link refusal signature does not verify")]
    BadSignature,
}

impl LinkRefusalError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::OtherLink => "otherLink",
            Self::OtherRequester => "otherRequester",
            Self::BadKey => "badKey",
            Self::BadSignature => "badSignature",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn inviter() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    const LINK: [u8; LINK_ID_LEN] = [0x11; LINK_ID_LEN];
    const B: [u8; 32] = [0x22; 32];

    #[test]
    fn a_composed_refusal_has_the_one_length_and_verifies() {
        let wire = compose_link_refusal(&inviter(), &LINK, &B);
        assert_eq!(wire.len(), LINK_REFUSAL_LEN);
        assert_eq!(LINK_REFUSAL_LEN, 135);
        let pk = inviter().verifying_key().to_bytes();
        let got = verify_link_refusal(&wire, &pk, &LINK, &B).unwrap();
        assert_eq!(got.link_id, LINK);
        assert_eq!(got.requester_account_key, B);
    }

    #[test]
    fn a_refusal_not_signed_by_the_inviter_is_refused() {
        let other = SigningKey::from_seed(&[8u8; 32]);
        let wire = compose_link_refusal(&other, &LINK, &B);
        let pk = inviter().verifying_key().to_bytes();
        assert_eq!(
            verify_link_refusal(&wire, &pk, &LINK, &B),
            Err(LinkRefusalError::BadSignature)
        );
    }

    #[test]
    fn a_refusal_for_another_link_or_requester_is_refused() {
        let wire = compose_link_refusal(&inviter(), &LINK, &B);
        let pk = inviter().verifying_key().to_bytes();
        assert_eq!(
            verify_link_refusal(&wire, &pk, &[0x12; LINK_ID_LEN], &B),
            Err(LinkRefusalError::OtherLink)
        );
        assert_eq!(
            verify_link_refusal(&wire, &pk, &LINK, &[0x23; 32]),
            Err(LinkRefusalError::OtherRequester)
        );
    }

    #[test]
    fn version_is_judged_before_length() {
        let mut wire = compose_link_refusal(&inviter(), &LINK, &B);
        wire[LINK_REFUSAL_DOMAIN_V1.len()] = 0x02;
        wire.truncate(40);
        assert_eq!(
            decode_link_refusal(&wire),
            Err(LinkRefusalError::UnknownVersion(0x02))
        );
        assert_eq!(
            decode_link_refusal(LINK_REFUSAL_DOMAIN_V1),
            Err(LinkRefusalError::BadLength { actual: 0 })
        );
        assert_eq!(
            decode_link_refusal(b"lakko/link-claim/v1\x00"),
            Err(LinkRefusalError::WrongDomain)
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::link_founded::LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            b"lakko/sync/link-record/v1\0",
        ];
        for d in other_domains {
            assert!(
                !d.starts_with(LINK_REFUSAL_DOMAIN_V1) && !LINK_REFUSAL_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
    }

    proptest! {
        #[test]
        fn any_single_byte_change_is_refused(i in 0usize..LINK_REFUSAL_LEN, flip in 1u8..=255) {
            let mut wire = compose_link_refusal(&inviter(), &LINK, &B);
            wire[i] ^= flip;
            let pk = inviter().verifying_key().to_bytes();
            prop_assert!(verify_link_refusal(&wire, &pk, &LINK, &B).is_err());
        }

        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            if let Ok(r) = decode_link_refusal(&bytes) {
                let mut again = signed_bytes(&r.link_id, &r.requester_account_key);
                again.extend_from_slice(&r.signature);
                prop_assert_eq!(again, bytes);
            }
        }
    }
}
