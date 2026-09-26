// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Profile request — a newcomer's "introduce yourselves" (ADR-0048 §2,
//! trigger 4).
//!
//! ## The problem this solves
//!
//! ADR-0048 carries a person's name to a group by **push only**: a device
//! publishes its profile when it joins, when the name changes, and when it
//! sees the group grow. The last trigger is what a newcomer depends on for
//! everyone else's name, and it fires on the *other* devices, each of which
//! has to notice the growth. A device that is away when the join lands
//! never re-sends, a web tab that is closed cannot, and one whose growth
//! re-send did not arrive has no second chance. The newcomer sits with
//! `Member` in place of a name and nothing it can do about it — seen live
//! on 2026-09-19, three of five members unnamed on a device that had
//! joined last.
//!
//! This envelope is the newcomer's pull: it asks once, and every member
//! answers with its ordinary profile envelope through the same throttled,
//! jittered publisher a growth re-send uses. A member that is away answers
//! when it next reads the journal, which is exactly when it could not have
//! noticed the growth either.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       PROFILE_REQUEST_ENVELOPE_DOMAIN_V1 || version_byte
//!      └────────── 25 bytes, ASCII ─────────┘ └── 0x01 ──┘
//!
//!                  "lakko/profile-request/v1\x00"
//! ```
//!
//! A fixed 26 bytes and **no fields**, for the reasons the committer
//! beacon gives: the asker is already MLS-authenticated through the sender
//! leaf, so naming itself would add an unauthenticated claim next to the
//! authenticated one; a timestamp would be the sender's clock where every
//! receiver already reads the DS row's `created_at`; and the answer is the
//! existing profile envelope, which needs nothing from the request. The
//! version byte is the forward-compatibility knob (the ADR-0044 v2 bump
//! showed it load-bearing on a sibling envelope).
//!
//! ## What the receiver does
//!
//! A member that reads a request publishes its profile into that group,
//! through the growth path (throttled to one per group per
//! `PROFILE_REPUBLISH_MIN_INTERVAL`, jittered) so N members answering one
//! request do not land N envelopes in the same second. A device that holds
//! no profile at all (no name and no username) has nothing to answer with
//! and stays silent, as it does on growth.
//!
//! ## When to ask — [`should_request_profiles`]
//!
//! Twice: right after joining a group, and on opening a group in which
//! some member still has no profile on this device, at most once per group
//! per [`PROFILE_REQUEST_MIN_INTERVAL_MS`]. The second trigger is what
//! heals groups that were joined before this envelope existed, and what
//! keeps a request from repeating while the answers are simply not back
//! yet. The rule lives here so the three clients cannot drift on it.
//!
//! Invariants: the request rides an E2EE MLS application message, so the
//! Delivery Service sees ciphertext (#2); it is never rendered as a chat
//! bubble (dispatched as its own [`crate::IncomingEnvelope::ProfileRequest`]
//! variant ahead of the intro fallback); a malformed request is dropped,
//! never answered (#4).

use core::result::Result as CoreResult;

use thiserror::Error;

/// 25-byte domain separator prepended to the payload. Frozen: it is the
/// `incoming_envelope` dispatch prefix, so a future revision moves the
/// version byte, never these bytes.
pub const PROFILE_REQUEST_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/profile-request/v1\x00";

/// Schema version carried in the single payload byte.
pub const PROFILE_REQUEST_VERSION_V1: u8 = 0x01;

/// Payload length after the domain separator: the version byte alone.
pub const PROFILE_REQUEST_PAYLOAD_LEN: usize = 1;

/// Whole-envelope length, what a background filter's cheap pre-check
/// compares against.
pub const PROFILE_REQUEST_ENVELOPE_LEN: usize =
    PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.len() + PROFILE_REQUEST_PAYLOAD_LEN;

/// How often one device may ask one group at most, outside a join.
/// Twenty-four hours: an answer that has not come within a day is a member
/// that has not read the journal in a day, and asking again sooner cannot
/// change that. A join always asks regardless.
pub const PROFILE_REQUEST_MIN_INTERVAL_MS: u64 = 24 * 60 * 60 * 1_000;

/// A decoded profile request. Carries nothing: the request's whole content
/// is that it arrived, from the MLS-authenticated sender leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProfileRequest;

/// Compose the request envelope. Infallible and byte-identical on every
/// call and for every sender.
#[must_use]
pub fn compose_profile_request() -> Vec<u8> {
    let mut out = Vec::with_capacity(PROFILE_REQUEST_ENVELOPE_LEN);
    out.extend_from_slice(PROFILE_REQUEST_ENVELOPE_DOMAIN_V1);
    out.push(PROFILE_REQUEST_VERSION_V1);
    out
}

/// Classify `plaintext`: `Ok(None)` when it does not carry this domain,
/// `Ok(Some(_))` for a well-formed request, `Err` for the domain with a
/// malformed payload (which a receiver drops rather than answers).
pub fn try_unwrap_profile_request(
    plaintext: &[u8],
) -> CoreResult<Option<ProfileRequest>, ProfileRequestError> {
    if !plaintext.starts_with(PROFILE_REQUEST_ENVELOPE_DOMAIN_V1) {
        return Ok(None);
    }
    let payload = &plaintext[PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.len()..];
    if payload.len() != PROFILE_REQUEST_PAYLOAD_LEN {
        return Err(ProfileRequestError::BadLength {
            expected: PROFILE_REQUEST_PAYLOAD_LEN,
            got: payload.len(),
        });
    }
    if payload[0] != PROFILE_REQUEST_VERSION_V1 {
        return Err(ProfileRequestError::UnknownVersion(payload[0]));
    }
    Ok(Some(ProfileRequest))
}

/// As [`try_unwrap_profile_request`], but a foreign domain is an error.
pub fn decode_profile_request(bytes: &[u8]) -> CoreResult<ProfileRequest, ProfileRequestError> {
    match try_unwrap_profile_request(bytes)? {
        Some(request) => Ok(request),
        None => Err(ProfileRequestError::WrongDomain),
    }
}

/// Should this device ask the group to introduce itself now?
///
/// * `joined_just_now` — the device has just joined the group: always ask,
///   whatever it asked before (a new membership is a new audience).
/// * `unnamed_members` — how many members of the group this device holds
///   no profile for, the viewer excluded. Nothing to ask when zero.
/// * `last_requested_at_ms` — when this device last asked this group,
///   `None` if never; the request repeats no sooner than
///   [`PROFILE_REQUEST_MIN_INTERVAL_MS`]. A clock that went backwards reads
///   as "recently asked" (the saturating difference is zero), never as a
///   reason to ask again.
#[must_use]
pub fn should_request_profiles(
    joined_just_now: bool,
    unnamed_members: usize,
    last_requested_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    if joined_just_now {
        return true;
    }
    if unnamed_members == 0 {
        return false;
    }
    last_requested_at_ms.map_or(true, |last| {
        now_ms.saturating_sub(last) >= PROFILE_REQUEST_MIN_INTERVAL_MS
    })
}

/// Why a plaintext that starts with the request domain was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProfileRequestError {
    /// The bytes do not carry the profile-request domain separator.
    #[error("plaintext does not carry the profile-request domain separator")]
    WrongDomain,

    /// The payload after the separator is not exactly the version byte.
    #[error("profile-request payload is {got} bytes, expected exactly {expected}")]
    BadLength {
        /// The one length a v1 payload can have.
        expected: usize,
        /// The length seen.
        got: usize,
    },

    /// The version byte names a schema this build does not know.
    #[error("profile-request payload carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_is_byte_identical_every_time() {
        let a = compose_profile_request();
        let b = compose_profile_request();
        assert_eq!(a, b);
        assert_eq!(a.len(), PROFILE_REQUEST_ENVELOPE_LEN);
        assert_eq!(decode_profile_request(&a), Ok(ProfileRequest));
        assert_eq!(try_unwrap_profile_request(&a), Ok(Some(ProfileRequest)));
    }

    /// The literals are the point: a background filter pins the length and
    /// the first byte, and a wire revision must fail here, not there.
    #[test]
    fn golden_pins_the_prefilter_shape() {
        let wire = compose_profile_request();
        assert_eq!(wire.len(), 26);
        assert_eq!(wire[0], 0x6c);
        assert_eq!(&wire[..25], b"lakko/profile-request/v1\x00");
        assert_eq!(wire[25], 0x01);
    }

    #[test]
    fn a_foreign_domain_is_not_a_request() {
        assert_eq!(try_unwrap_profile_request(b"hello"), Ok(None));
        assert_eq!(try_unwrap_profile_request(&[]), Ok(None));
        assert_eq!(
            decode_profile_request(b"hello"),
            Err(ProfileRequestError::WrongDomain)
        );
        // The domain minus its NUL is a prefix of nothing here: a chat that
        // happens to start with the ASCII text is still not a request.
        assert_eq!(
            try_unwrap_profile_request(b"lakko/profile-request/v1 is a fine subject"),
            Ok(None)
        );
    }

    #[test]
    fn a_malformed_payload_is_refused_not_ignored() {
        let bare = PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
        assert_eq!(
            try_unwrap_profile_request(&bare),
            Err(ProfileRequestError::BadLength {
                expected: 1,
                got: 0
            })
        );
        let mut long = compose_profile_request();
        long.push(0x00);
        assert_eq!(
            try_unwrap_profile_request(&long),
            Err(ProfileRequestError::BadLength {
                expected: 1,
                got: 2
            })
        );
        let mut v2 = PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(
            try_unwrap_profile_request(&v2),
            Err(ProfileRequestError::UnknownVersion(0x02))
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
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
        ];
        for other in other_domains {
            assert_ne!(*other, PROFILE_REQUEST_ENVELOPE_DOMAIN_V1);
            assert!(
                !other.starts_with(PROFILE_REQUEST_ENVELOPE_DOMAIN_V1)
                    && !PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.starts_with(other),
                "{other:?} is prefix-related to the profile-request domain",
            );
        }
    }

    #[test]
    fn request_does_not_cross_dispatch_as_a_sibling() {
        use crate::committer_beacon::try_unwrap_committer_beacon;
        use crate::group_name::try_unwrap_group_name;
        use crate::profile::try_unwrap_profile;

        let wire = compose_profile_request();
        assert!(try_unwrap_committer_beacon(&wire).unwrap().is_none());
        assert!(try_unwrap_profile(&wire).unwrap().is_none());
        assert!(try_unwrap_group_name(&wire).unwrap().is_none());
        // ...and the profile envelope, the request's answer, is not a request.
        let beacon = crate::committer_beacon::compose_committer_beacon();
        assert_eq!(try_unwrap_profile_request(&beacon), Ok(None));
    }

    #[test]
    fn asks_on_join_regardless_of_history() {
        assert!(should_request_profiles(true, 0, Some(10), 10));
        assert!(should_request_profiles(true, 0, None, 0));
    }

    #[test]
    fn asks_on_open_only_for_an_unnamed_member_and_at_most_daily() {
        let day = PROFILE_REQUEST_MIN_INTERVAL_MS;
        assert!(
            !should_request_profiles(false, 0, None, 1_000),
            "nobody unnamed"
        );
        assert!(
            should_request_profiles(false, 1, None, 1_000),
            "never asked"
        );
        assert!(!should_request_profiles(
            false,
            1,
            Some(1_000),
            1_000 + day - 1
        ));
        assert!(should_request_profiles(false, 1, Some(1_000), 1_000 + day));
        // A clock jump backwards is "recently asked", never a reason to ask.
        assert!(!should_request_profiles(false, 1, Some(5_000), 1_000));
    }
}
