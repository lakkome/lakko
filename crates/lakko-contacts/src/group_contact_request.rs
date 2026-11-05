// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A contact request carried inside a group — ADR-0086 section 6, built
//! ahead of the rest of that ADR (UX `docs/ux/who-can-find-me.md` 6.4).
//!
//! A person can stand on a device as a contact, or be a co-member of a
//! group, while the device holds no reach grant that writes to that person's
//! mailbox. Starting a one-to-one needs the person to accept a contact
//! request, and a request normally travels to the mailbox. When no grant
//! reaches the mailbox and both are current members of a group, the request
//! goes **inside the group** instead: an MLS application message carrying
//! this envelope. Only a current member can write there, so a person who
//! left cannot be asked through it.
//!
//! ## Other members must not learn whom it is for (maintainer, 2026-11-05)
//!
//! The envelope names nobody. The whole ordinary contact request is sealed
//! to the target's X25519 key with the crate's sealed box, under labels of
//! its own, so a member who is not the target sees only that the sender
//! posted a request-sized box. The inner request in the clear carries the
//! sender's key and account id and a random `from_pseudonym` (ADR-0078), and
//! a sealed envelope that names no recipient; nothing in it names the
//! target, but sealing it whole leaves nothing to examine at all.
//!
//! Addressing is decided by the receiver: every member tries to open the box
//! with its own X25519 secret, and a member whose open fails drops it
//! silently ([`GroupContactRequestError::NotForMe`]). A damaged box reads the
//! same way, which is the sealed box's property.
//!
//! ## Wire format
//!
//! ```text
//! domain(31) "lakko/group-contact-request/v1\0" ‖ version(1) = 0x01
//!   ‖ sealed_len(4, big-endian) ‖ sealed(sealed_len)
//! sealed = eph_pub(32) ‖ ChaCha20-Poly1305(contact request wire) ‖ tag(16)
//!          AAD = domain ‖ version
//! ```
//!
//! The sealed request is the ordinary [`crate::request`] wire — the bytes
//! [`crate::request::compose`] produces and
//! [`crate::request::decode_and_verify`] consumes. It is 1 to
//! [`GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN`] bytes, the largest body a
//! mailbox row carries ([`crate::mailbox::MAILBOX_MAX_BODY_LEN`]): the most
//! any contact request can be in transit today. So `sealed_len` is
//! [`GROUP_CONTACT_REQUEST_MIN_SEALED_LEN`] to
//! [`GROUP_CONTACT_REQUEST_MAX_SEALED_LEN`].
//!
//! [`decode_group_contact_request`] is structural (layout and bounds), what
//! the dispatcher and the notification extension run.
//! [`check_group_contact_request`] is the receiver's rule.
//!
//! ## The receiver's rule
//!
//! Every member of the group receives the envelope. A member's device:
//!
//! 1. drops it unless the box opens with its own X25519 secret;
//! 2. drops it unless the opened request decodes and its signature holds;
//! 3. drops it unless the request's `from_pubkey` is the account key the
//!    MLS sender leaf resolves to at receipt (C-68) — so a member cannot
//!    relay another person's request through a group the other person is
//!    not writing in.
//!
//! What follows is the client's: the person's "Who can find me" setting
//! must let members of this group find them (`VisibilitySetting::allows_scope`),
//! or the sender must already be a contact; otherwise the request is dropped
//! silently. Then it joins the ordinary incoming-request flow (freshness,
//! open, accept through the request's reply grant).
//!
//! ## What other members still learn
//!
//! That the sender posted one such box, when, and its length. The length
//! follows the request (profile, intro); it is not padded.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it. The seal
//! labels differ from the contact request's and the mailbox's, so none of
//! the three boxes opens as another.

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{open_labeled, seal_labeled, SealLabels, EPH_PUB_LEN, TAG_LEN};
use crate::mailbox::MAILBOX_MAX_BODY_LEN;
use crate::request::{decode_and_verify, ContactRequest};

/// 31-byte domain separator. Frozen: the clients route a group service
/// envelope on this exact prefix; the in-payload version byte is the knob
/// that moves.
pub const GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-contact-request/v1\x00";

/// Schema-version byte carried first after the separator.
pub const GROUP_CONTACT_REQUEST_VERSION_V1: u8 = 0x01;

/// Longest sealed request: the largest mailbox row body, the most a contact
/// request can be in transit.
pub const GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN: usize = MAILBOX_MAX_BODY_LEN;

/// Shortest sealed box (a one-byte request).
pub const GROUP_CONTACT_REQUEST_MIN_SEALED_LEN: usize = EPH_PUB_LEN + 1 + TAG_LEN;

/// Longest sealed box.
pub const GROUP_CONTACT_REQUEST_MAX_SEALED_LEN: usize =
    EPH_PUB_LEN + GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN + TAG_LEN;

/// Bytes after the separator before the box: version, box length.
const FIXED_PAYLOAD_LEN: usize = 1 + 4;

/// Shortest well-formed envelope.
pub const GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN: usize = GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1
    .len()
    + FIXED_PAYLOAD_LEN
    + GROUP_CONTACT_REQUEST_MIN_SEALED_LEN;

/// Longest well-formed envelope.
pub const GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN: usize = GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1
    .len()
    + FIXED_PAYLOAD_LEN
    + GROUP_CONTACT_REQUEST_MAX_SEALED_LEN;

/// The seal labels of this box: distinct from the contact request's own
/// seal and from the mailbox row's.
const SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko-contacts/v1/group-contact-request/key",
    nonce: b"lakko-contacts/v1/group-contact-request/nonce",
};

/// The AAD the box is sealed under: the separator and the version byte.
fn seal_aad() -> Vec<u8> {
    let mut aad = GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
    aad.push(GROUP_CONTACT_REQUEST_VERSION_V1);
    aad
}

/// A structurally decoded envelope: one sealed box, addressed to nobody in
/// the clear.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GroupContactRequest {
    /// The sealed box, [`GROUP_CONTACT_REQUEST_MIN_SEALED_LEN`] to
    /// [`GROUP_CONTACT_REQUEST_MAX_SEALED_LEN`] bytes.
    pub sealed: Vec<u8>,
}

impl core::fmt::Debug for GroupContactRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GroupContactRequest")
            .field("sealed_len", &self.sealed.len())
            .finish_non_exhaustive()
    }
}

/// Seal an ordinary contact request (`request`, the bytes
/// [`crate::request::compose`] returned) to the target's X25519 public key
/// and frame it: the plaintext of an application message in a group both
/// people are in.
///
/// # Errors
///
/// * [`GroupContactRequestError::RequestLength`] when the request is empty
///   or longer than [`GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN`].
/// * [`GroupContactRequestError::Seal`] when the sealed box could not be
///   built (an AEAD failure; not expected with valid inputs).
pub fn seal_group_contact_request(
    recipient_x25519_pub: &X25519PublicKey,
    request: &[u8],
) -> CoreResult<Vec<u8>, GroupContactRequestError> {
    let len = request.len();
    if len == 0 || len > GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN {
        return Err(GroupContactRequestError::RequestLength { len });
    }
    let sealed = seal_labeled(&SEAL_LABELS, recipient_x25519_pub, &seal_aad(), request)
        .map_err(|_| GroupContactRequestError::Seal)?;
    compose_group_contact_request(&GroupContactRequest { sealed })
}

/// Frame an already-sealed box. [`decode_group_contact_request`] accepts
/// exactly what this produces.
///
/// # Errors
///
/// [`GroupContactRequestError::SealedLength`] when the box is outside
/// [`GROUP_CONTACT_REQUEST_MIN_SEALED_LEN`] to
/// [`GROUP_CONTACT_REQUEST_MAX_SEALED_LEN`] (what the decoder refuses).
pub fn compose_group_contact_request(
    envelope: &GroupContactRequest,
) -> CoreResult<Vec<u8>, GroupContactRequestError> {
    let len = envelope.sealed.len();
    if !(GROUP_CONTACT_REQUEST_MIN_SEALED_LEN..=GROUP_CONTACT_REQUEST_MAX_SEALED_LEN).contains(&len)
    {
        return Err(GroupContactRequestError::SealedLength { len });
    }
    let len32 = u32::try_from(len).map_err(|_| GroupContactRequestError::SealedLength { len })?;
    let mut out = Vec::with_capacity(
        GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + len,
    );
    out.extend_from_slice(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1);
    out.push(GROUP_CONTACT_REQUEST_VERSION_V1);
    out.extend_from_slice(&len32.to_be_bytes());
    out.extend_from_slice(&envelope.sealed);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(envelope))` — the separator matched and the layout is
///   well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed; dropped.
///
/// # Errors
///
/// * [`GroupContactRequestError::UnknownVersion`] — checked before any
///   length.
/// * [`GroupContactRequestError::BadLength`] — the payload is shorter than
///   its fixed part, or the box does not fill exactly the rest.
/// * [`GroupContactRequestError::SealedLength`] — the declared box length is
///   outside its bounds.
pub fn try_unwrap_group_contact_request(
    plaintext: &[u8],
) -> CoreResult<Option<GroupContactRequest>, GroupContactRequestError> {
    let Some(payload) = plaintext.strip_prefix(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let bad = || GroupContactRequestError::BadLength {
        actual: payload.len(),
    };
    let (&version, rest) = payload.split_first().ok_or_else(bad)?;
    if version != GROUP_CONTACT_REQUEST_VERSION_V1 {
        return Err(GroupContactRequestError::UnknownVersion(version));
    }
    if rest.len() < FIXED_PAYLOAD_LEN - 1 {
        return Err(bad());
    }
    let (len_bytes, sealed) = rest.split_at(4);
    let declared_u32 = u32::from_be_bytes(len_bytes.try_into().map_err(|_| bad())?);
    let declared = usize::try_from(declared_u32).unwrap_or(usize::MAX);
    if !(GROUP_CONTACT_REQUEST_MIN_SEALED_LEN..=GROUP_CONTACT_REQUEST_MAX_SEALED_LEN)
        .contains(&declared)
    {
        return Err(GroupContactRequestError::SealedLength { len: declared });
    }
    if sealed.len() != declared {
        return Err(bad());
    }
    Ok(Some(GroupContactRequest {
        sealed: sealed.to_vec(),
    }))
}

/// Strict standalone decoder: a plaintext of another kind is a hard error.
///
/// # Errors
///
/// * [`GroupContactRequestError::WrongDomain`] — `bytes` does not start with
///   [`GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_group_contact_request`] returns.
pub fn decode_group_contact_request(
    bytes: &[u8],
) -> CoreResult<GroupContactRequest, GroupContactRequestError> {
    try_unwrap_group_contact_request(bytes)?.ok_or(GroupContactRequestError::WrongDomain)
}

/// The receiver's rule (module docs): the box opens with `own_x25519_priv`,
/// the request inside decodes and its signature holds, and the request is
/// from `sender_account_key` — the account key the MLS sender leaf of the
/// carrying message resolves to at receipt (C-68). Returns the verified
/// request and its wire; the client then applies its "Who can find me"
/// setting and hands the request to the ordinary incoming-request flow.
///
/// # Errors
///
/// * [`GroupContactRequestError::NotForMe`] — the box does not open with
///   this key (another member's, or damaged; the two read alike). Drop it
///   silently and log nothing that names anyone.
/// * [`GroupContactRequestError::RequestInvalid`] — the opened request does
///   not decode or its signature fails.
/// * [`GroupContactRequestError::SenderMismatch`] — the request names
///   another sender than the member who posted it.
pub fn check_group_contact_request(
    envelope: &GroupContactRequest,
    own_x25519_priv: &X25519SecretKey,
    sender_account_key: &[u8; 32],
) -> CoreResult<(ContactRequest, Vec<u8>), GroupContactRequestError> {
    let wire = open_labeled(&SEAL_LABELS, own_x25519_priv, &seal_aad(), &envelope.sealed)
        .map_err(|_| GroupContactRequestError::NotForMe)?;
    let request = decode_and_verify(&wire).map_err(|_| GroupContactRequestError::RequestInvalid)?;
    if &request.from_pubkey.to_bytes() != sender_account_key {
        return Err(GroupContactRequestError::SenderMismatch);
    }
    Ok((request, wire))
}

/// Why an envelope was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupContactRequestError {
    /// The plaintext does not carry the envelope's domain separator. Only
    /// from [`decode_group_contact_request`].
    #[error("plaintext does not carry the group-contact-request domain separator")]
    WrongDomain,
    /// The payload does not have the layout's length.
    #[error("group-contact-request envelope has {actual} payload bytes, which is not its layout")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The request to seal is empty or above
    /// [`GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN`].
    #[error(
        "group-contact-request request of {len} bytes is outside \
         1..={GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN}"
    )]
    RequestLength {
        /// The given request length.
        len: usize,
    },
    /// The box length is outside its bounds.
    #[error(
        "group-contact-request box of {len} bytes is outside \
         {GROUP_CONTACT_REQUEST_MIN_SEALED_LEN}..={GROUP_CONTACT_REQUEST_MAX_SEALED_LEN}"
    )]
    SealedLength {
        /// The declared (or given) box length.
        len: usize,
    },
    /// The version byte is not [`GROUP_CONTACT_REQUEST_VERSION_V1`].
    #[error("group-contact-request envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The sealed box could not be built.
    #[error("group-contact-request box could not be sealed")]
    Seal,
    /// The box does not open with this key: not for this member, or
    /// damaged.
    #[error("group-contact-request box is not for this member")]
    NotForMe,
    /// The opened contact request does not decode or its signature fails.
    #[error("group-contact-request box carries an invalid contact request")]
    RequestInvalid,
    /// The opened request names another sender than the member who posted
    /// it.
    #[error("group-contact-request box was posted by someone other than its sender")]
    SenderMismatch,
}

impl GroupContactRequestError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::RequestLength { .. } => "requestLength",
            Self::SealedLength { .. } => "sealedLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::Seal => "seal",
            Self::NotForMe => "notForMe",
            Self::RequestInvalid => "requestInvalid",
            Self::SenderMismatch => "senderMismatch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{compose, ACCOUNT_ID_LEN};
    use lakko_crypto::signing::SigningKey;
    use proptest::prelude::*;

    fn sender() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    fn target() -> X25519SecretKey {
        X25519SecretKey::from_bytes([0xCD; 32])
    }

    fn other_member() -> X25519SecretKey {
        X25519SecretKey::from_bytes([0xCE; 32])
    }

    fn real_request(signer: &SigningKey) -> Vec<u8> {
        compose(
            signer,
            &[0x77; ACCOUNT_ID_LEN],
            &target().public_key(),
            [0xAA; crate::pseudonym::PSEUDONYM_LEN],
            1_000_000,
            b"",
            None,
            None,
            None,
            b"",
        )
        .unwrap()
    }

    #[test]
    fn domain_separator_and_bounds_are_frozen() {
        assert_eq!(
            GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1,
            b"lakko/group-contact-request/v1\x00"
        );
        assert_eq!(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1.len(), 31);
        assert_eq!(GROUP_CONTACT_REQUEST_MIN_SEALED_LEN, 32 + 1 + 16);
        assert_eq!(GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN, 31 + 1 + 4 + 49);
        assert_eq!(
            GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN,
            31 + 1 + 4 + 32 + MAILBOX_MAX_BODY_LEN + 16
        );
    }

    #[test]
    fn the_target_opens_it_and_the_wire_names_nobody() {
        let alice = sender();
        let request = real_request(&alice);
        let wire = seal_group_contact_request(&target().public_key(), &request).unwrap();
        // Neither the target's keys nor anything of the request is in the clear.
        let target_pub = target().public_key().to_bytes();
        assert!(!wire.windows(32).any(|w| w == target_pub));
        assert!(!wire
            .windows(32)
            .any(|w| w == alice.verifying_key().to_bytes()));
        assert!(!wire.windows(request.len()).any(|w| w == request.as_slice()));
        let envelope = decode_group_contact_request(&wire).unwrap();
        assert_eq!(compose_group_contact_request(&envelope).unwrap(), wire);
        let (opened, opened_wire) =
            check_group_contact_request(&envelope, &target(), &alice.verifying_key().to_bytes())
                .unwrap();
        assert_eq!(
            opened.from_pubkey.to_bytes(),
            alice.verifying_key().to_bytes()
        );
        assert_eq!(opened_wire, request);
    }

    #[test]
    fn another_member_cannot_open_it() {
        let alice = sender();
        let wire =
            seal_group_contact_request(&target().public_key(), &real_request(&alice)).unwrap();
        let envelope = decode_group_contact_request(&wire).unwrap();
        assert_eq!(
            check_group_contact_request(
                &envelope,
                &other_member(),
                &alice.verifying_key().to_bytes()
            )
            .unwrap_err(),
            GroupContactRequestError::NotForMe
        );
    }

    #[test]
    fn a_relayed_or_damaged_request_is_refused() {
        let alice = sender();
        let alice_key = alice.verifying_key().to_bytes();
        let wire =
            seal_group_contact_request(&target().public_key(), &real_request(&alice)).unwrap();
        let envelope = decode_group_contact_request(&wire).unwrap();
        // Posted by a member who is not its author.
        assert_eq!(
            check_group_contact_request(&envelope, &target(), &[0x99; 32]).unwrap_err(),
            GroupContactRequestError::SenderMismatch
        );
        // A damaged box reads as not for me.
        let mut damaged = envelope.clone();
        let last = damaged.sealed.len() - 1;
        damaged.sealed[last] ^= 0x01;
        assert_eq!(
            check_group_contact_request(&damaged, &target(), &alice_key).unwrap_err(),
            GroupContactRequestError::NotForMe
        );
        // A box that opens but carries no valid request.
        let junk = seal_group_contact_request(&target().public_key(), &[1, 2, 3]).unwrap();
        assert_eq!(
            check_group_contact_request(
                &decode_group_contact_request(&junk).unwrap(),
                &target(),
                &alice_key
            )
            .unwrap_err(),
            GroupContactRequestError::RequestInvalid
        );
    }

    #[test]
    fn the_box_does_not_open_as_a_contact_request_seal() {
        // Same recipient key, labels of its own: the contact request's own
        // sealed-box open refuses it.
        let wire = seal_group_contact_request(&target().public_key(), &[5; 40]).unwrap();
        let envelope = decode_group_contact_request(&wire).unwrap();
        assert!(crate::envelope::open(&target(), &seal_aad(), &envelope.sealed).is_err());
    }

    #[test]
    fn sealing_refuses_what_decode_refuses() {
        let pk = target().public_key();
        assert_eq!(
            seal_group_contact_request(&pk, &[]),
            Err(GroupContactRequestError::RequestLength { len: 0 })
        );
        let over = GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN + 1;
        assert_eq!(
            seal_group_contact_request(&pk, &vec![0; over]),
            Err(GroupContactRequestError::RequestLength { len: over })
        );
        let max = seal_group_contact_request(&pk, &vec![7; GROUP_CONTACT_REQUEST_MAX_REQUEST_LEN])
            .unwrap();
        assert_eq!(max.len(), GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN);
        assert!(decode_group_contact_request(&max).is_ok());
        let short = GROUP_CONTACT_REQUEST_MIN_SEALED_LEN - 1;
        assert_eq!(
            compose_group_contact_request(&GroupContactRequest {
                sealed: vec![0; short]
            }),
            Err(GroupContactRequestError::SealedLength { len: short })
        );
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_decode_refuses_it() {
        assert_eq!(try_unwrap_group_contact_request(b"hello"), Ok(None));
        assert_eq!(
            decode_group_contact_request(b"hello"),
            Err(GroupContactRequestError::WrongDomain)
        );
        assert_eq!(
            try_unwrap_group_contact_request(&real_request(&sender())),
            Ok(None)
        );
    }

    #[test]
    fn a_bare_separator_is_an_error_not_a_fall_through() {
        assert!(matches!(
            try_unwrap_group_contact_request(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1),
            Err(GroupContactRequestError::BadLength { actual: 0 })
        ));
    }

    #[test]
    fn version_is_checked_before_length() {
        let mut wire = seal_group_contact_request(&target().public_key(), &[5; 10]).unwrap();
        wire[31] = 0x02;
        wire.truncate(34);
        assert_eq!(
            try_unwrap_group_contact_request(&wire),
            Err(GroupContactRequestError::UnknownVersion(0x02))
        );
    }

    #[test]
    fn a_wrong_box_length_is_refused() {
        let wire = seal_group_contact_request(&target().public_key(), &[5; 10]).unwrap();
        assert!(matches!(
            try_unwrap_group_contact_request(&wire[..wire.len() - 1]),
            Err(GroupContactRequestError::BadLength { .. })
        ));
        let mut long = wire.clone();
        long.push(0);
        assert!(matches!(
            try_unwrap_group_contact_request(&long),
            Err(GroupContactRequestError::BadLength { .. })
        ));
        let at = GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1.len() + 1;
        let mut zero = wire.clone();
        zero[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            try_unwrap_group_contact_request(&zero),
            Err(GroupContactRequestError::SealedLength { len: 0 })
        );
        let mut over = wire;
        over[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            try_unwrap_group_contact_request(&over),
            Err(GroupContactRequestError::SealedLength { .. })
        ));
        let short = [GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1, &[0x01, 0, 0][..]].concat();
        assert!(matches!(
            try_unwrap_group_contact_request(&short),
            Err(GroupContactRequestError::BadLength { .. })
        ));
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::link_founded::LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            b"lakko/sync/link-record/v1\0",
            b"lakko/sync/lineage/v1\0",
            b"lakko/sync/recovery-email/v1\0",
            b"lakko/sync/own-username/v1\0",
        ];
        for d in other_domains {
            assert_ne!(*d, GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1)
                    && !GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    proptest! {
        /// Decode is total: any input either decodes to an envelope that
        /// re-composes to exactly the input, or is refused; never a panic.
        /// Checking it under a fixed key never panics either.
        #[test]
        fn decode_is_total_and_canonical(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(e) = decode_group_contact_request(&bytes) {
                prop_assert_eq!(compose_group_contact_request(&e).unwrap(), bytes);
                prop_assert_eq!(
                    check_group_contact_request(&e, &target(), &[0; 32]).unwrap_err(),
                    GroupContactRequestError::NotForMe
                );
            }
        }

        /// Every sealable request round-trips through the target's open.
        #[test]
        fn every_sealed_request_opens_for_the_target(
            request in prop::collection::vec(any::<u8>(), 1..2048),
        ) {
            let wire = seal_group_contact_request(&target().public_key(), &request).unwrap();
            prop_assert!(wire.len() >= GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN);
            prop_assert!(wire.len() <= GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN);
            let e = decode_group_contact_request(&wire).unwrap();
            let opened = open_labeled(&SEAL_LABELS, &target(), &seal_aad(), &e.sealed).unwrap();
            prop_assert_eq!(opened, request);
        }
    }
}
