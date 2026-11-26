// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-closed envelope — an admin tells every member that this group is
//! closed, and where to continue
//! ([ADR-0073](../../../docs/adr/0073-account-key-reset.md) phase 2;
//! `docs/ux/identity-recovery.md` §16.5 / §16.8).
//!
//! ## Why this exists
//!
//! When a group's owner resets their account keys, the group can never be
//! made secure again: the owner's old devices cannot be removed
//! (`owner_survives`) and nobody can become its new owner (a transfer needs
//! the owner's key, which is gone). The UX decision is **Recreate group**:
//! an admin creates a new group, re-invites the people who are still here,
//! and posts a **closed** notice into the old one naming the new group. The
//! notice is the one new wire artefact of that section; it is
//! client-to-client and the server never parses it.
//!
//! ## What travels, and where
//!
//! A closed notice is `{closed_at_ms, successor_group_id}`. It rides as an
//! MLS application message on the old group's own journal, classified by
//! [`crate::incoming_envelope`] so it is never rendered as a chat bubble.
//! The server sees only ciphertext under the group's epoch secret
//! (invariants #1, #2); the successor's id is inside the sealed payload, so
//! no server learns that two journals are the same people (invariant #5).
//!
//! It is the [`crate::group_name`] template applied to a different fact:
//! one bounded, fail-closed record, one stamp, one strict order. The
//! differences: the variable field is a group id rather than a name
//! (exactly 0 or [`SUCCESSOR_GROUP_ID_LEN`] bytes, so there is no character
//! table), and compose is infallible, since every value of [`GroupClosed`]
//! is encodable.
//!
//! ## What the envelope does NOT establish
//!
//! **A closed notice is a fact one person asserts about the whole group.**
//! This module bounds the shape of that assertion; it does not decide
//! whether the sender was entitled to make it:
//!
//! * **Authority is the RECEIVER's check, through the role table**
//!   (§16.5: "from an admin of the group, resolved at receipt; a notice
//!   from a plain member is dropped"). The receiver resolves the MLS
//!   sender leaf to the account key at receipt (C-68) and reads the live
//!   ADR-0022 role table: owner or admin → accept if newer; plain member →
//!   drop; unresolvable sender or unreadable table → drop. The envelope
//!   deliberately carries no identity and no role of its own — a role named
//!   in the payload would be an unauthenticated claim beside the
//!   MLS-authenticated one, and the only correct reading of it is to
//!   ignore it.
//! * The gate runs **before** the order comparison, and the whole
//!   read-decide-write runs under the per-group lock the group name and the
//!   push routing secret already use (C-62 sub-invariant 5: a gate must
//!   read current state).
//! * The successor id is unverifiable by construction: the receiver treats
//!   it as "where the admin says to continue", and `Open the new group` is
//!   offered only once this device has actually joined that group through
//!   the ordinary invitation path. A notice can never admit anyone
//!   anywhere.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       GROUP_CLOSED_ENVELOPE_DOMAIN_V1 || version || closed_at_ms || successor_len || successor_group_id
//!      └──────── 22 bytes, ASCII ───────┘ └─ 0x01 ─┘ └── 8 B, BE ──┘ └── 1 B ─────┘ └─ 0 or 32 B ────┘
//!
//!                     "lakko/group-closed/v1\x00"
//! ```
//!
//! A `successor_len` of `0` means "closed without a successor" (the admin
//! closed the group and recreated nothing); any other value than
//! [`SUCCESSOR_GROUP_ID_LEN`] is refused. Total length is exactly
//! [`GROUP_CLOSED_ENVELOPE_MIN_LEN`] or [`GROUP_CLOSED_ENVELOPE_MAX_LEN`].
//!
//! ## Order — and the stamp must be re-emitted VERBATIM
//!
//! [`group_closed_is_newer`]: a receiver keeps the notice with the greater
//! `closed_at_ms` **per group**; equal is not newer. The stamp is the
//! closer's own clock; ties keep what is stored. Two admins may each
//! recreate (§16.5: "the client does not coordinate"): the newer stamp wins
//! the banner's target on every device, both new groups exist.
//!
//! The ADR-0044 lesson applies without change: a **growth re-send** (an
//! admin re-broadcasts the current notice when a member joins the closed
//! group, so a newcomer learns it is closed) must carry the ORIGINAL stamp
//! — the stamp is durable per `(account, group)` state, persisted beside
//! the successor id and re-emitted unchanged. A re-send that re-stamps the
//! current time makes an older notice arrive **newer** than a second
//! recreate that landed in between, and every member's `Open` target flips
//! back. This module owns the order; persisting and replaying the stamp
//! verbatim is the caller's obligation.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it, since
//! [`crate::incoming_envelope`] dispatches on a byte-prefix match.

use core::result::Result as CoreResult;

use thiserror::Error;

/// 22-byte domain separator prepended to the group-closed payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with, mirroring the sibling envelopes. The printable
/// prefix is `lakko/group-closed/v1`.
///
/// **The `v1` names the envelope FAMILY, not the schema version.** These
/// bytes are frozen: [`crate::incoming_envelope`] dispatches on this exact
/// prefix. The in-payload version byte is the knob that moves.
pub const GROUP_CLOSED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-closed/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const GROUP_CLOSED_VERSION_V1: u8 = 0x01;

/// Length of a successor group id when one is carried. An MLS group id in
/// Lakko is 32 random bytes; the envelope admits exactly that or nothing.
pub const SUCCESSOR_GROUP_ID_LEN: usize = 32;

/// Bytes after the separator that every envelope carries: version,
/// `closed_at_ms`, `successor_len`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1;

/// Shortest well-formed envelope: closed without a successor.
pub const GROUP_CLOSED_ENVELOPE_MIN_LEN: usize =
    GROUP_CLOSED_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Longest well-formed envelope: closed with a successor. There is no
/// length in between — `successor_len` is 0 or [`SUCCESSOR_GROUP_ID_LEN`].
pub const GROUP_CLOSED_ENVELOPE_MAX_LEN: usize =
    GROUP_CLOSED_ENVELOPE_MIN_LEN + SUCCESSOR_GROUP_ID_LEN;

/// A group's closed notice, as posted by an owner or admin.
///
/// **The notice is the closer's assertion about the whole group.** See the
/// module docs: the receiver decides through the role table whether the
/// sender may make it, and through [`group_closed_is_newer`] whether it is
/// current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupClosed {
    /// Closer-clock UNIX milliseconds at which the group was closed. Orders
    /// notices for the same group via [`group_closed_is_newer`]. Persisted
    /// and re-emitted verbatim on a growth re-send (see the module docs).
    pub closed_at_ms: u64,
    /// The recreated group's id, where the members continue; `None` when
    /// the group was closed without a successor. **Unverifiable by
    /// construction** — the receiver offers `Open the new group` only once
    /// it has joined that group through the ordinary invitation path.
    pub successor_group_id: Option<[u8; SUCCESSOR_GROUP_ID_LEN]>,
}

/// `true` when a notice stamped `incoming_closed_at_ms` must replace one
/// stamped `stored_closed_at_ms` for the **same group**.
///
/// Strictly greater wins; **equal is not newer**, so a growth re-send of the
/// notice a receiver already holds is not an update, and two recreates with
/// one stamp keep what is stored.
#[must_use]
pub const fn group_closed_is_newer(incoming_closed_at_ms: u64, stored_closed_at_ms: u64) -> bool {
    incoming_closed_at_ms > stored_closed_at_ms
}

/// Compose a group-closed envelope.
///
/// The output is the **inner** plaintext handed to the MLS
/// application-message send path on the old group's journal. Infallible:
/// every [`GroupClosed`] is encodable, so compose can refuse nothing that
/// [`decode_group_closed`] would accept.
#[must_use]
pub fn compose_group_closed(closed: &GroupClosed) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_CLOSED_ENVELOPE_MAX_LEN);
    out.extend_from_slice(GROUP_CLOSED_ENVELOPE_DOMAIN_V1);
    out.push(GROUP_CLOSED_VERSION_V1);
    out.extend_from_slice(&closed.closed_at_ms.to_be_bytes());
    match closed.successor_group_id {
        Some(id) => {
            // `SUCCESSOR_GROUP_ID_LEN` is 32, which fits a byte; written as a
            // checked conversion rather than a cast so a future bound change
            // past 255 cannot truncate silently. The fallback is unreachable.
            out.push(u8::try_from(SUCCESSOR_GROUP_ID_LEN).unwrap_or(u8::MAX));
            out.extend_from_slice(&id);
        },
        None => out.push(0),
    }
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(closed))` — the domain separator matched and the envelope is
///   well-formed and within every bound.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed or out of
///   bounds. The caller drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`GroupClosedError::Truncated`] — the payload ends before a declared
///   field.
/// * [`GroupClosedError::UnknownVersion`] — the version byte is not
///   [`GROUP_CLOSED_VERSION_V1`]. Checked BEFORE the remaining length, so a
///   later revision that changes the length reports as a version, not as a
///   truncation.
/// * [`GroupClosedError::BadSuccessorLen`] — `successor_len` is neither 0
///   nor [`SUCCESSOR_GROUP_ID_LEN`].
/// * [`GroupClosedError::TrailingBytes`] — bytes remain after the successor.
pub fn try_unwrap_group_closed(
    plaintext: &[u8],
) -> CoreResult<Option<GroupClosed>, GroupClosedError> {
    let Some(payload) = plaintext.strip_prefix(GROUP_CLOSED_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;

    let version = take_u8(&mut cur)?;
    if version != GROUP_CLOSED_VERSION_V1 {
        return Err(GroupClosedError::UnknownVersion(version));
    }
    let closed_at_bytes: [u8; 8] = take(&mut cur, 8)?
        .try_into()
        .map_err(|_| GroupClosedError::Truncated)?;
    let closed_at_ms = u64::from_be_bytes(closed_at_bytes);

    let successor_len = take_u8(&mut cur)?;
    let successor_group_id = match usize::from(successor_len) {
        0 => None,
        SUCCESSOR_GROUP_ID_LEN => {
            let id: [u8; SUCCESSOR_GROUP_ID_LEN] = take(&mut cur, SUCCESSOR_GROUP_ID_LEN)?
                .try_into()
                .map_err(|_| GroupClosedError::Truncated)?;
            Some(id)
        },
        _ => return Err(GroupClosedError::BadSuccessorLen(successor_len)),
    };
    if !cur.is_empty() {
        return Err(GroupClosedError::TrailingBytes { count: cur.len() });
    }

    Ok(Some(GroupClosed {
        closed_at_ms,
        successor_group_id,
    }))
}

/// Strict standalone decoder: a non-group-closed plaintext is a hard error.
///
/// # Errors
///
/// * [`GroupClosedError::WrongDomain`] — `bytes` does not start with
///   [`GROUP_CLOSED_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_group_closed`] returns.
pub fn decode_group_closed(bytes: &[u8]) -> CoreResult<GroupClosed, GroupClosedError> {
    try_unwrap_group_closed(bytes)?.ok_or(GroupClosedError::WrongDomain)
}

/// Why a group-closed envelope was refused.
///
/// A reason code: no variant carries the successor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupClosedError {
    /// The plaintext does not carry the group-closed domain separator. Only
    /// from [`decode_group_closed`]; [`try_unwrap_group_closed`] returns
    /// `Ok(None)`.
    #[error("plaintext does not carry the group-closed domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("group-closed envelope is truncated")]
    Truncated,
    /// Bytes remain after the successor field.
    #[error("group-closed envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the successor.
        count: usize,
    },
    /// The version byte is not [`GROUP_CLOSED_VERSION_V1`].
    #[error("group-closed envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// `successor_len` is neither 0 nor [`SUCCESSOR_GROUP_ID_LEN`].
    #[error("group-closed envelope declares a successor id of {0} bytes")]
    BadSuccessorLen(u8),
}

impl GroupClosedError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::Truncated => "truncated",
            Self::TrailingBytes { .. } => "trailingBytes",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadSuccessorLen(_) => "badSuccessorLen",
        }
    }
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, GroupClosedError> {
    let (&first, rest) = cur.split_first().ok_or(GroupClosedError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], GroupClosedError> {
    if cur.len() < n {
        return Err(GroupClosedError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const fn closed(at: u64, successor: Option<[u8; SUCCESSOR_GROUP_ID_LEN]>) -> GroupClosed {
        GroupClosed {
            closed_at_ms: at,
            successor_group_id: successor,
        }
    }

    /// Hand-build an envelope with arbitrary (possibly invalid) fields,
    /// bypassing `compose` — so decode's own gates are tested.
    fn raw(version: u8, at: u64, successor_len: u8, successor: &[u8]) -> Vec<u8> {
        let mut out = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&at.to_be_bytes());
        out.push(successor_len);
        out.extend_from_slice(successor);
        out
    }

    // --- wire format ---------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        for c in [
            closed(1_700_000_000_000, Some([0x5A; 32])),
            closed(0, None),
            closed(u64::MAX, Some([0x00; 32])),
            closed(1, Some([0xFF; 32])),
        ] {
            let wire = compose_group_closed(&c);
            assert_eq!(decode_group_closed(&wire).unwrap(), c);
            assert_eq!(try_unwrap_group_closed(&wire).unwrap(), Some(c));
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        let id: [u8; 32] = core::array::from_fn(|i| u8::try_from(i).unwrap());
        let wire = compose_group_closed(&closed(0x0102_0304_0506_0708, Some(id)));
        let sep = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(sep, 22);
        assert_eq!(&wire[..sep], b"lakko/group-closed/v1\x00");
        assert_eq!(wire[sep], GROUP_CLOSED_VERSION_V1);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 32);
        assert_eq!(&wire[sep + 10..], &id);
        assert_eq!(wire[0], b'l');
    }

    #[test]
    fn envelope_length_bounds_are_the_documented_construction() {
        assert_eq!(GROUP_CLOSED_ENVELOPE_MIN_LEN, 32);
        assert_eq!(GROUP_CLOSED_ENVELOPE_MAX_LEN, 64);
        assert_eq!(
            compose_group_closed(&closed(0, None)).len(),
            GROUP_CLOSED_ENVELOPE_MIN_LEN
        );
        assert_eq!(
            compose_group_closed(&closed(0, Some([1; 32]))).len(),
            GROUP_CLOSED_ENVELOPE_MAX_LEN
        );
    }

    // --- the missing successor ---------------------------------------------

    #[test]
    fn closed_without_a_successor_round_trips() {
        let wire = compose_group_closed(&closed(42, None));
        let decoded = decode_group_closed(&wire).unwrap();
        assert_eq!(decoded.successor_group_id, None);
        assert_eq!(decoded.closed_at_ms, 42);
        assert_eq!(
            decode_group_closed(&raw(1, 42, 0, &[])),
            Ok(closed(42, None))
        );
    }

    // --- fail-closed decode --------------------------------------------------

    #[test]
    fn non_group_closed_plaintext_falls_through() {
        assert_eq!(try_unwrap_group_closed(b"hi alice"), Ok(None));
        assert_eq!(try_unwrap_group_closed(&[]), Ok(None));
        assert_eq!(
            decode_group_closed(b"hi alice"),
            Err(GroupClosedError::WrongDomain)
        );
    }

    #[test]
    fn bare_separator_is_truncated_not_a_fall_through() {
        assert_eq!(
            try_unwrap_group_closed(GROUP_CLOSED_ENVELOPE_DOMAIN_V1),
            Err(GroupClosedError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_group_closed(&closed(9, Some([7; 32])));
        for cut in GROUP_CLOSED_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_group_closed(&wire[..cut]),
                Err(GroupClosedError::Truncated),
                "cut at {cut}"
            );
        }
        let short = compose_group_closed(&closed(9, None));
        for cut in GROUP_CLOSED_ENVELOPE_DOMAIN_V1.len()..short.len() {
            assert_eq!(
                try_unwrap_group_closed(&short[..cut]),
                Err(GroupClosedError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut wire = compose_group_closed(&closed(9, Some([7; 32])));
        wire.push(0);
        assert_eq!(
            decode_group_closed(&wire),
            Err(GroupClosedError::TrailingBytes { count: 1 })
        );
        // Also on the no-successor envelope, where a stray byte could be
        // misread as part of an id if the length were not consulted.
        let mut empty = compose_group_closed(&closed(9, None));
        empty.extend_from_slice(&[0xAB, 0xCD]);
        assert_eq!(
            decode_group_closed(&empty),
            Err(GroupClosedError::TrailingBytes { count: 2 })
        );
    }

    #[test]
    fn unknown_versions_are_refused_before_the_length_is_consulted() {
        for v in [0x00, 0x02, 0xFF] {
            assert_eq!(
                decode_group_closed(&raw(v, 0, 0, &[])),
                Err(GroupClosedError::UnknownVersion(v))
            );
        }
        // A future-version envelope of a different length reports as a
        // version, not as a truncation.
        let mut v2 = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(
            try_unwrap_group_closed(&v2),
            Err(GroupClosedError::UnknownVersion(0x02))
        );
    }

    #[test]
    fn a_successor_of_any_other_length_is_refused() {
        for len in [1u8, 16, 31, 33, 64, 255] {
            let body = vec![0x11; usize::from(len)];
            assert_eq!(
                decode_group_closed(&raw(1, 0, len, &body)),
                Err(GroupClosedError::BadSuccessorLen(len)),
                "len {len}"
            );
        }
        // The length byte is judged before the bytes are read: a declared
        // 16 with no bytes behind it is still a bad length, not a truncation.
        assert_eq!(
            decode_group_closed(&raw(1, 0, 16, &[])),
            Err(GroupClosedError::BadSuccessorLen(16))
        );
    }

    #[test]
    fn error_messages_never_echo_the_successor() {
        let id = [0x77; 32];
        let mut wire = compose_group_closed(&closed(0, Some(id)));
        wire.push(0);
        let err = decode_group_closed(&wire).unwrap_err();
        assert!(!err.to_string().contains("77"));
        assert!(!format!("{err:?}").contains("77"));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(GroupClosedError::WrongDomain.code(), "wrongDomain");
        assert_eq!(GroupClosedError::Truncated.code(), "truncated");
        assert_eq!(
            GroupClosedError::TrailingBytes { count: 3 }.code(),
            "trailingBytes"
        );
        assert_eq!(GroupClosedError::UnknownVersion(9).code(), "unknownVersion");
        assert_eq!(
            GroupClosedError::BadSuccessorLen(9).code(),
            "badSuccessorLen"
        );
    }

    // --- order ---------------------------------------------------------------

    #[test]
    fn newer_is_strictly_greater() {
        assert!(group_closed_is_newer(2, 1));
        assert!(!group_closed_is_newer(1, 1), "equal is not newer");
        assert!(!group_closed_is_newer(0, 1));
        assert!(group_closed_is_newer(u64::MAX, u64::MAX - 1));
        assert!(!group_closed_is_newer(u64::MAX, u64::MAX));
    }

    #[test]
    fn a_growth_resend_with_the_original_stamp_never_clobbers_a_later_recreate() {
        // The ADR-0044 lesson, on this envelope: the closer persists the
        // stamp and re-emits it VERBATIM. Ivan recreated at t=10 naming
        // group A; Maria recreated at t=20 naming group B; Ivan's growth
        // re-send still carries 10 and so loses on every receiver, whose
        // `Open the new group` keeps pointing at B.
        let first = closed(10, Some([0xA; 32]));
        let second = closed(20, Some([0xB; 32]));
        let resend = decode_group_closed(&compose_group_closed(&first)).unwrap();
        assert_eq!(resend.closed_at_ms, 10);
        assert!(!group_closed_is_newer(
            resend.closed_at_ms,
            second.closed_at_ms
        ));
        // Had the re-send been re-stamped at "now" (t=30), it would win —
        // exactly the defect the verbatim rule exists to prevent.
        assert!(group_closed_is_newer(30, second.closed_at_ms));
    }

    // --- cross-kind isolation ------------------------------------------------

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
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, GROUP_CLOSED_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(GROUP_CLOSED_ENVELOPE_DOMAIN_V1)
                    && !GROUP_CLOSED_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn group_closed_does_not_cross_dispatch_as_a_sibling() {
        use crate::call_kp_forward::{try_unwrap_call_kp_forward, wrap_call_kp_forward};
        use crate::call_welcome::{try_unwrap_call_welcome, wrap_call_welcome};
        use crate::committer_beacon::{compose_committer_beacon, try_unwrap_committer_beacon};
        use crate::group_name::{compose_group_name, try_unwrap_group_name, GroupName};
        use crate::media_state::{compose_media_state, try_unwrap_media_state};
        use crate::pin::{compose_pin, try_unwrap_pin, PinAction, PinOp};
        use crate::profile::{compose_profile, try_unwrap_profile, Profile};
        use crate::push_routing::{compose_push_routing_secret, try_unwrap_push_routing_secret};

        let wire = compose_group_closed(&closed(1, Some([3; 32])));
        assert!(try_unwrap_call_welcome(&wire).unwrap().is_none());
        assert!(try_unwrap_call_kp_forward(&wire).unwrap().is_none());
        assert!(try_unwrap_media_state(&wire).unwrap().is_none());
        assert!(try_unwrap_push_routing_secret(&wire).unwrap().is_none());
        assert!(try_unwrap_committer_beacon(&wire).unwrap().is_none());
        assert!(try_unwrap_profile(&wire).unwrap().is_none());
        assert!(try_unwrap_group_name(&wire).unwrap().is_none());
        assert!(try_unwrap_pin(&wire).unwrap().is_none());

        let profile = Profile {
            display_name: Some("Alice".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 1,
            account_id: None,
            username_certificate: None,
            grants: Vec::new(),
            avatar: None,
            card_shareable: true,
        };
        for sibling in [
            wrap_call_welcome(&[0xAB; 256]),
            wrap_call_kp_forward(&[0xCD; 512]),
            compose_media_state(true, true),
            compose_push_routing_secret(7, &[0x11; 32]),
            compose_committer_beacon(),
            compose_profile(
                &profile,
                &lakko_crypto::signing::SigningKey::from_seed(&[1; 32]),
            )
            .unwrap(),
            compose_group_name(&GroupName {
                name: "Trio".to_string(),
                updated_at_ms: 1,
            })
            .unwrap(),
            compose_pin(&PinOp {
                message_id: 1,
                action: PinAction::Pin,
                updated_at_ms: 1,
            }),
        ] {
            assert_eq!(try_unwrap_group_closed(&sibling), Ok(None));
        }
    }
}
