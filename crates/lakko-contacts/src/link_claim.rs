// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Link-request claim — one device of the inviter's account claims a contact
//! request sent through one of the account's invite links, so that only one
//! device answers it
//! ([ADR-0079](../../../docs/adr/0079-invite-link-makes-contacts.md), as
//! reworked 2026-10-17; `docs/ux/contact-invite-link.md` section 6).
//!
//! ## Why this exists
//!
//! Every device of the inviter reads the one account mailbox, so two devices
//! can see the same request through a live link before either one has counted
//! the use in the synced link record. Each would then found a one-to-one with
//! the requester. The claim makes it one, with the claim-then-build pattern
//! ADR-0073 R.6 uses for successor groups, here on the account's own
//! DeviceGroup journal:
//!
//! 1. a device that decides to accept posts a claim naming the request into
//!    its DeviceGroup and notes the DS message id the publish returned; it
//!    builds nothing yet;
//! 2. it reads its DeviceGroup journal past that id: the EARLIEST claim for
//!    the same request from a device in the current roster wins, and the
//!    device whose leaf posted it builds ([`link_claim_outcome`]). A device
//!    recognises its own claims by its own leaf key (it records the sealed
//!    claim before the publish and matches the row's bytes, so a crash that
//!    lost the publish's answer does not lose the claim), and its later
//!    duplicate claims change nothing. A claim from a device no longer in the
//!    roster is passed over, so a removed device cannot leave a request
//!    unanswered;
//! 3. only the winner founds the one-to-one, sends the Welcome, records the
//!    contact and counts the use;
//! 4. a device that holds no claim of its own and sees a claim from a device
//!    in the roster treats the request as answered.
//!
//! Journal order is the Delivery Service's arrival order, which every device
//! reads identically, so every device with the same roster reaches the same
//! winner.
//!
//! ## Wire format (v1)
//!
//! ```text
//! LINK_CLAIM_ENVELOPE_DOMAIN_V1 || version(1) || link_id(16) || requester_account_key(32)
//! ```
//!
//! Fixed length: [`LINK_CLAIM_ENVELOPE_LEN`] bytes. It rides the DeviceGroup's
//! opaque `ServiceEnvelope` carrier, end-to-end encrypted to the account's own
//! devices; the server sees a DeviceGroup row and nothing about the request
//! (invariants #2, #5).
//!
//! ## What the envelope does NOT establish
//!
//! * **It names nothing about its sender.** The sender is the MLS-authenticated
//!   DeviceGroup leaf, resolved at receipt (C-68). A claim is honoured only
//!   from a device of the same account, which in the DeviceGroup is every
//!   resolvable member; a claim met in any other group is dropped.
//! * **It carries no proof the request exists.** A device of the account that
//!   claims a request nobody sent only stops its own account's devices from
//!   answering a request that does not exist.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::invite::LINK_ID_LEN;

/// 20-byte domain separator prepended to the claim payload. The trailing
/// `\x00` keeps a UTF-8 free-text payload from ever starting with it.
///
/// **The `v1` names the envelope FAMILY.** These bytes are frozen: the
/// clients route a DeviceGroup service envelope on this exact prefix; the
/// in-payload version byte is the knob that moves.
pub const LINK_CLAIM_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/link-claim/v1\x00";

/// Schema-version byte carried first after the separator.
pub const LINK_CLAIM_VERSION_V1: u8 = 0x01;

/// Length of the requester's account key (Ed25519 public key).
pub const LINK_CLAIM_REQUESTER_KEY_LEN: usize = 32;

/// Length of the request key [`LinkClaim::request_key`]: `link_id ‖
/// requester_account_key`.
pub const LINK_CLAIM_REQUEST_KEY_LEN: usize = LINK_ID_LEN + LINK_CLAIM_REQUESTER_KEY_LEN;

/// Bytes after the separator: version, link id, requester key.
const PAYLOAD_LEN: usize = 1 + LINK_CLAIM_REQUEST_KEY_LEN;

/// The one well-formed length: the envelope has no variable field.
pub const LINK_CLAIM_ENVELOPE_LEN: usize = LINK_CLAIM_ENVELOPE_DOMAIN_V1.len() + PAYLOAD_LEN;

/// One claim: the request it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkClaim {
    /// The id of the invite link the request answered (the request's link
    /// answer).
    pub link_id: [u8; LINK_ID_LEN],
    /// The requester's account key (the key the request's signature
    /// authenticates).
    pub requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN],
}

impl LinkClaim {
    /// The request key: `link_id(16) ‖ requester_account_key(32)`, the one
    /// name every client keys its claim bookkeeping on.
    #[must_use]
    pub fn request_key(&self) -> [u8; LINK_CLAIM_REQUEST_KEY_LEN] {
        let mut out = [0u8; LINK_CLAIM_REQUEST_KEY_LEN];
        out[..LINK_ID_LEN].copy_from_slice(&self.link_id);
        out[LINK_ID_LEN..].copy_from_slice(&self.requester_account_key);
        out
    }
}

/// Compose a claim envelope: the plaintext handed to the DeviceGroup's
/// `ServiceEnvelope` carrier. Every [`LinkClaim`] is encodable, so this cannot
/// fail, and [`decode_link_claim`] accepts exactly what this produces.
#[must_use]
pub fn compose_link_claim(claim: &LinkClaim) -> Vec<u8> {
    let mut out = Vec::with_capacity(LINK_CLAIM_ENVELOPE_LEN);
    out.extend_from_slice(LINK_CLAIM_ENVELOPE_DOMAIN_V1);
    out.push(LINK_CLAIM_VERSION_V1);
    out.extend_from_slice(&claim.link_id);
    out.extend_from_slice(&claim.requester_account_key);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(claim))` — the separator matched and the envelope is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed. The caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`LinkClaimError::UnknownVersion`] — the version byte is not
///   [`LINK_CLAIM_VERSION_V1`]. Checked BEFORE the length, so a later revision
///   that changes the length reports as a version, not as a truncation.
/// * [`LinkClaimError::BadLength`] — the payload is not exactly
///   [`PAYLOAD_LEN`] bytes.
pub fn try_unwrap_link_claim(plaintext: &[u8]) -> CoreResult<Option<LinkClaim>, LinkClaimError> {
    let Some(payload) = plaintext.strip_prefix(LINK_CLAIM_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(LinkClaimError::BadLength {
        actual: payload.len(),
    })?;
    if version != LINK_CLAIM_VERSION_V1 {
        return Err(LinkClaimError::UnknownVersion(version));
    }
    if rest.len() != LINK_CLAIM_REQUEST_KEY_LEN {
        return Err(LinkClaimError::BadLength {
            actual: payload.len(),
        });
    }
    let bad = || LinkClaimError::BadLength {
        actual: payload.len(),
    };
    let link_id: [u8; LINK_ID_LEN] = rest[..LINK_ID_LEN].try_into().map_err(|_| bad())?;
    let requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN] =
        rest[LINK_ID_LEN..].try_into().map_err(|_| bad())?;
    Ok(Some(LinkClaim {
        link_id,
        requester_account_key,
    }))
}

/// Strict standalone decoder: a non-claim plaintext is a hard error.
///
/// # Errors
///
/// * [`LinkClaimError::WrongDomain`] — `bytes` does not start with
///   [`LINK_CLAIM_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_link_claim`] returns.
pub fn decode_link_claim(bytes: &[u8]) -> CoreResult<LinkClaim, LinkClaimError> {
    try_unwrap_link_claim(bytes)?.ok_or(LinkClaimError::WrongDomain)
}

/// Why a claim envelope was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkClaimError {
    /// The plaintext does not carry the claim domain separator. Only from
    /// [`decode_link_claim`]; [`try_unwrap_link_claim`] returns `Ok(None)`.
    #[error("plaintext does not carry the link-claim domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("link-claim envelope has {actual} payload bytes, expected {PAYLOAD_LEN}")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The version byte is not [`LINK_CLAIM_VERSION_V1`].
    #[error("link-claim envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl LinkClaimError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
        }
    }
}

/// Length of a DeviceGroup leaf signature key (Ed25519): the author of a
/// claim as the receiver resolves it from the MLS-authenticated sender leaf.
pub const LINK_CLAIM_AUTHOR_KEY_LEN: usize = 32;

/// One claim for a request as a device holds it: where it sits in the
/// DeviceGroup journal and which device posted it.
///
/// The author is never read from the envelope. For another device's claim it
/// is the MLS-authenticated sender leaf, resolved at receipt (C-68). For a
/// claim of this device it is this device's own leaf key, recorded either
/// from the publish's answer or, when the device lost that answer (a crash
/// between the publish and the record), by matching the journal row's bytes
/// against the sealed claim it recorded BEFORE the publish: a device cannot
/// decrypt its own MLS application messages, so the bytes are how it
/// recognises its own row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObservedLinkClaim {
    /// The DS message id of the claim's row: the order.
    pub message_id: u64,
    /// The DeviceGroup leaf signature key of the device that posted it.
    pub author_key: [u8; LINK_CLAIM_AUTHOR_KEY_LEN],
}

/// How a request's claims end, for one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClaimOutcome {
    /// The winning claim is this device's (the earliest of the device's
    /// claims, if it posted several): this device answers the request.
    Won {
        /// The DS message id of the winning claim.
        message_id: u64,
    },
    /// Another device's claim wins: this device treats the request as
    /// answered and builds nothing.
    Lost {
        /// The DS message id of the winning claim.
        winner_message_id: u64,
    },
    /// No claim from a device in the current roster is held: nobody has won
    /// yet (this device's own claim did not land, or every claim came from a
    /// device since removed). The caller posts (again) and decides later.
    NoWinner,
}

/// The winning claim for ONE request: the earliest claim (lowest DS message
/// id) whose author is in `roster_keys`, the DeviceGroup's CURRENT leaf
/// signature keys. A claim from a device that is no longer in the roster (a
/// removed device) is passed over, so a removed device cannot leave a request
/// unanswered; the next earliest claim from a current device wins instead.
/// Journal order is the DS's arrival order, which every device reads
/// identically. `None` when no claim comes from the roster.
#[must_use]
pub fn link_claim_winner(
    claims: &[ObservedLinkClaim],
    roster_keys: &[[u8; LINK_CLAIM_AUTHOR_KEY_LEN]],
) -> Option<ObservedLinkClaim> {
    claims
        .iter()
        .filter(|c| roster_keys.contains(&c.author_key))
        .min_by_key(|c| c.message_id)
        .copied()
}

/// How the claims for one request end for the device whose leaf key is
/// `own_key`: the earliest claim from a current roster device wins
/// ([`link_claim_winner`]), and the device whose leaf posted it builds.
///
/// A device whose own earlier claim is in `claims` wins whether or not it
/// recorded that claim's id at publish time (it may have recognised the row
/// by its bytes after a crash), and its later duplicate claims change
/// nothing. A device that is not in `roster_keys` itself can never win.
///
/// The caller passes every claim it holds for the request (its own included,
/// with its own leaf key as the author), read off the DeviceGroup journal up
/// to at least its own latest claim, and holds its per-group serialization
/// over the read and the write of the outcome (C-62 sub-invariant 5).
///
/// | claims from the roster              | outcome                    |
/// |-------------------------------------|----------------------------|
/// | none                                | `NoWinner`                 |
/// | earliest is this device's           | `Won { its id }`           |
/// | earliest is another device's        | `Lost { its id }`          |
#[must_use]
pub fn link_claim_outcome(
    own_key: &[u8; LINK_CLAIM_AUTHOR_KEY_LEN],
    claims: &[ObservedLinkClaim],
    roster_keys: &[[u8; LINK_CLAIM_AUTHOR_KEY_LEN]],
) -> LinkClaimOutcome {
    match link_claim_winner(claims, roster_keys) {
        None => LinkClaimOutcome::NoWinner,
        Some(w) if &w.author_key == own_key => LinkClaimOutcome::Won {
            message_id: w.message_id,
        },
        Some(w) => LinkClaimOutcome::Lost {
            winner_message_id: w.message_id,
        },
    }
}

/// Whether the request is answered for a device that holds NO claim of its
/// own: some device in the current roster claimed it. A claim only from a
/// removed device does not answer it.
#[must_use]
pub fn link_claim_answered_elsewhere(
    claims: &[ObservedLinkClaim],
    roster_keys: &[[u8; LINK_CLAIM_AUTHOR_KEY_LEN]],
) -> bool {
    link_claim_winner(claims, roster_keys).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn claim(id: u8, key: u8) -> LinkClaim {
        LinkClaim {
            link_id: [id; LINK_ID_LEN],
            requester_account_key: [key; LINK_CLAIM_REQUESTER_KEY_LEN],
        }
    }

    #[test]
    fn compose_then_decode_round_trips_and_is_fixed_length() {
        for c in [claim(0, 0), claim(1, 2), claim(0xFF, 0xFE)] {
            let bytes = compose_link_claim(&c);
            assert_eq!(bytes.len(), LINK_CLAIM_ENVELOPE_LEN);
            assert_eq!(decode_link_claim(&bytes).unwrap(), c);
            assert_eq!(try_unwrap_link_claim(&bytes).unwrap(), Some(c));
        }
    }

    #[test]
    fn domain_separator_is_frozen() {
        assert_eq!(LINK_CLAIM_ENVELOPE_DOMAIN_V1, b"lakko/link-claim/v1\x00");
        assert_eq!(LINK_CLAIM_ENVELOPE_DOMAIN_V1.len(), 20);
        assert_eq!(LINK_CLAIM_ENVELOPE_LEN, 20 + 1 + 16 + 32);
        assert_eq!(compose_link_claim(&claim(1, 2))[0], b'l');
    }

    #[test]
    fn the_request_key_is_the_link_id_then_the_requester_key() {
        let c = claim(7, 9);
        let key = c.request_key();
        assert_eq!(&key[..16], &[7; 16]);
        assert_eq!(&key[16..], &[9; 32]);
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_decode_refuses_it() {
        assert_eq!(try_unwrap_link_claim(b"hello").unwrap(), None);
        assert_eq!(
            decode_link_claim(b"hello"),
            Err(LinkClaimError::WrongDomain)
        );
        assert_eq!(try_unwrap_link_claim(b"").unwrap(), None);
    }

    #[test]
    fn a_bare_separator_is_an_error_not_a_fall_through() {
        assert_eq!(
            try_unwrap_link_claim(LINK_CLAIM_ENVELOPE_DOMAIN_V1),
            Err(LinkClaimError::BadLength { actual: 0 })
        );
    }

    #[test]
    fn version_is_checked_before_length() {
        let mut v2 = LINK_CLAIM_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(
            try_unwrap_link_claim(&v2),
            Err(LinkClaimError::UnknownVersion(0x02))
        );
        let mut v2_long = compose_link_claim(&claim(1, 1));
        v2_long[LINK_CLAIM_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        v2_long.push(0);
        assert_eq!(
            try_unwrap_link_claim(&v2_long),
            Err(LinkClaimError::UnknownVersion(0x02))
        );
    }

    #[test]
    fn every_short_or_long_payload_is_refused() {
        let good = compose_link_claim(&claim(3, 4));
        for cut in LINK_CLAIM_ENVELOPE_DOMAIN_V1.len()..good.len() {
            assert!(
                matches!(
                    try_unwrap_link_claim(&good[..cut]),
                    Err(LinkClaimError::BadLength { .. })
                ),
                "{cut}"
            );
        }
        let mut long = good;
        long.push(0);
        assert!(matches!(
            try_unwrap_link_claim(&long),
            Err(LinkClaimError::BadLength { .. })
        ));
    }

    fn obs(message_id: u64, author: u8) -> ObservedLinkClaim {
        ObservedLinkClaim {
            message_id,
            author_key: [author; LINK_CLAIM_AUTHOR_KEY_LEN],
        }
    }

    const A: [u8; 32] = [0xA; 32];
    const B: [u8; 32] = [0xB; 32];
    const C: [u8; 32] = [0xC; 32];

    #[test]
    fn the_earliest_roster_claim_wins_and_its_author_builds() {
        let roster = [A, B];
        assert_eq!(
            link_claim_outcome(&A, &[], &roster),
            LinkClaimOutcome::NoWinner
        );
        let claims = [obs(12, 0xB), obs(10, 0xA), obs(40, 0xB)];
        assert_eq!(
            link_claim_outcome(&A, &claims, &roster),
            LinkClaimOutcome::Won { message_id: 10 }
        );
        assert_eq!(
            link_claim_outcome(&B, &claims, &roster),
            LinkClaimOutcome::Lost {
                winner_message_id: 10
            }
        );
        assert_eq!(link_claim_winner(&claims, &roster), Some(obs(10, 0xA)));
        assert!(link_claim_answered_elsewhere(&claims, &roster));
        assert!(!link_claim_answered_elsewhere(&[], &roster));
    }

    #[test]
    fn a_crash_duplicate_keeps_the_earlier_own_claim_and_one_builder() {
        // A posted 10 and crashed before recording it; B posted 11; A, back,
        // posted 12. A recognised row 10 as its own by its bytes.
        let roster = [A, B];
        let seen_by_a = [obs(10, 0xA), obs(11, 0xB), obs(12, 0xA)];
        let seen_by_b = [obs(10, 0xA), obs(11, 0xB), obs(12, 0xA)];
        assert_eq!(
            link_claim_outcome(&A, &seen_by_a, &roster),
            LinkClaimOutcome::Won { message_id: 10 }
        );
        assert_eq!(
            link_claim_outcome(&B, &seen_by_b, &roster),
            LinkClaimOutcome::Lost {
                winner_message_id: 10
            }
        );
        // The later duplicate alone changes nothing either.
        assert_eq!(
            link_claim_outcome(&A, &[obs(12, 0xA), obs(10, 0xA)], &roster),
            LinkClaimOutcome::Won { message_id: 10 }
        );
    }

    #[test]
    fn a_removed_devices_earliest_claim_is_passed_over() {
        // C posted first and was then removed from the DeviceGroup.
        let roster = [A, B];
        let claims = [obs(5, 0xC), obs(7, 0xB), obs(9, 0xA)];
        assert_eq!(
            link_claim_outcome(&B, &claims, &roster),
            LinkClaimOutcome::Won { message_id: 7 }
        );
        assert_eq!(
            link_claim_outcome(&A, &claims, &roster),
            LinkClaimOutcome::Lost {
                winner_message_id: 7
            }
        );
        // A claim only from the removed device answers nothing.
        assert!(!link_claim_answered_elsewhere(&[obs(5, 0xC)], &roster));
        // A device outside the roster never wins, even with the earliest claim.
        assert_eq!(
            link_claim_outcome(&C, &claims, &roster),
            LinkClaimOutcome::Lost {
                winner_message_id: 7
            }
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
            // The lakko-sync registers that ride the same DeviceGroup carrier.
            b"lakko/sync/link-record/v1\0",
            b"lakko/sync/lineage/v1\0",
            b"lakko/sync/recovery-email/v1\0",
            b"lakko/sync/own-username/v1\0",
        ];
        for d in other_domains {
            assert_ne!(*d, LINK_CLAIM_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(LINK_CLAIM_ENVELOPE_DOMAIN_V1)
                    && !LINK_CLAIM_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any byte string decodes or is refused, never panics, and what
        /// decodes re-composes to the identical bytes.
        #[test]
        fn decode_is_total_and_canonical(bytes in prop::collection::vec(any::<u8>(), 0..120)) {
            let mut input = LINK_CLAIM_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(c) = decode_link_claim(&input) {
                prop_assert_eq!(compose_link_claim(&c), input);
            }
        }

        /// Every device reaches the same winner, whatever order it read the
        /// claims in and whatever duplicates it posted: exactly one device
        /// of the roster wins, it is the author of the earliest roster
        /// claim, and every other device of the roster loses to that claim.
        #[test]
        fn exactly_one_device_wins_across_any_interleaving(
            authors in prop::collection::vec(0u8..4, 1..10),
            ids in prop::collection::btree_set(1u64..10_000, 10),
            removed in 0u8..5,
            seed in any::<u64>(),
        ) {
            let ids: Vec<u64> = ids.into_iter().collect();
            let devices: Vec<[u8; 32]> = (0u8..4).map(|d| [d + 1; 32]).collect();
            // Device `removed` (when < 4) has left the roster.
            let roster: Vec<[u8; 32]> = devices
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != usize::from(removed))
                .map(|(_, k)| *k)
                .collect();
            let mut claims: Vec<ObservedLinkClaim> = authors
                .iter()
                .zip(ids.iter())
                .map(|(a, id)| ObservedLinkClaim {
                    message_id: *id,
                    author_key: devices[usize::from(*a)],
                })
                .collect();
            let n = claims.len();
            if n > 1 {
                claims.rotate_left(usize::try_from(seed % (n as u64)).unwrap_or(0));
            }
            let winner = link_claim_winner(&claims, &roster);
            let mut winners = 0;
            for key in &roster {
                match link_claim_outcome(key, &claims, &roster) {
                    LinkClaimOutcome::Won { message_id } => {
                        winners += 1;
                        prop_assert_eq!(Some(message_id), winner.map(|w| w.message_id));
                        prop_assert_eq!(Some(*key), winner.map(|w| w.author_key));
                    }
                    LinkClaimOutcome::Lost { winner_message_id } => {
                        prop_assert_eq!(Some(winner_message_id), winner.map(|w| w.message_id));
                    }
                    LinkClaimOutcome::NoWinner => prop_assert!(winner.is_none()),
                }
            }
            prop_assert_eq!(winners, usize::from(winner.is_some()));
            // The winner is a roster device and no roster claim precedes it.
            if let Some(w) = winner {
                prop_assert!(roster.contains(&w.author_key));
                prop_assert!(claims
                    .iter()
                    .filter(|c| roster.contains(&c.author_key))
                    .all(|c| c.message_id >= w.message_id));
            }
        }

        /// A crash duplicate: a device whose earlier claim was recognised by
        /// its bytes (not by a recorded id) still wins against another
        /// device's later claim, and exactly one device builds.
        #[test]
        fn an_unrecorded_earlier_own_claim_still_wins(
            first in 1u64..1_000,
            gap_other in 1u64..1_000,
            gap_dup in 1u64..1_000,
        ) {
            let roster = [A, B];
            let other = first + gap_other;
            let duplicate = other + gap_dup;
            let claims = [obs(first, 0xA), obs(other, 0xB), obs(duplicate, 0xA)];
            prop_assert_eq!(
                link_claim_outcome(&A, &claims, &roster),
                LinkClaimOutcome::Won { message_id: first }
            );
            prop_assert_eq!(
                link_claim_outcome(&B, &claims, &roster),
                LinkClaimOutcome::Lost { winner_message_id: first }
            );
        }

        /// A removed device's earliest claim never wins: the next earliest
        /// claim from a current device does, and its author builds.
        #[test]
        fn a_removed_devices_claim_hands_the_request_to_the_next_current_claim(
            removed_first in 1u64..1_000,
            gap_b in 1u64..1_000,
            gap_a in 1u64..1_000,
        ) {
            let roster = [A, B];
            let b_id = removed_first + gap_b;
            let a_id = b_id + gap_a;
            let claims = [obs(removed_first, 0xC), obs(a_id, 0xA), obs(b_id, 0xB)];
            prop_assert_eq!(
                link_claim_outcome(&B, &claims, &roster),
                LinkClaimOutcome::Won { message_id: b_id }
            );
            prop_assert_eq!(
                link_claim_outcome(&A, &claims, &roster),
                LinkClaimOutcome::Lost { winner_message_id: b_id }
            );
            prop_assert_eq!(
                link_claim_outcome(&C, &claims, &roster),
                LinkClaimOutcome::Lost { winner_message_id: b_id }
            );
        }
    }
}
