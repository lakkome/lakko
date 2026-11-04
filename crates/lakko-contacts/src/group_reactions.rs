// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group allowed-reactions envelope: which reactions a group's messages take,
//! as the owner or an admin set it, carried end-to-end encrypted to every
//! member (UX `docs/ux/reactions-and-emoji.md` sections 9 and 17 item 1).
//!
//! It is the [`crate::group_name`] template with an [`AllowedReactions`]
//! value in place of the name: one bounded, fail-closed envelope, one stamp,
//! one strict order. The value's encoding is shared with the channel
//! metadata record ([`crate::allowed_reactions`]), so a group and a channel
//! cannot disagree on what "a list of 1 to 100" means.
//!
//! ## What travels, and where
//!
//! An MLS application message on the group's own journal, classified by
//! [`crate::incoming_envelope`] so it is never rendered as a chat bubble.
//! The server sees ciphertext only (invariants #1, #2); no server-side table
//! says what a group allows (invariant #5).
//!
//! ## What the envelope does NOT establish
//!
//! * **Authority is the RECEIVER's check**, exactly as for the group name:
//!   resolve the MLS sender to the account key at receipt (C-68), read the
//!   live role table, accept from the owner or an admin (any member when the
//!   group has no role table), drop otherwise; the whole read-decide-write
//!   under the per-group lock (C-62 sub-invariant 5). The envelope names no
//!   sender and no role.
//! * **The value is a display rule, not an enforcement.** Nobody can refuse
//!   a reaction another member's client sends; receivers hide chips of
//!   reactions the group does not take and their composers do not offer them
//!   (UX D11). A reaction already given is not taken off anyone's list.
//!
//! ## Wire shape
//!
//! ```text
//!   GROUP_REACTIONS_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || allowed_reactions
//!   └──────── 25 bytes, ASCII ───────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └ crate::allowed_reactions ┘
//!
//!                     "lakko/group-reactions/v1\x00"
//! ```
//!
//! Total length is between [`GROUP_REACTIONS_ENVELOPE_MIN_LEN`] and
//! [`GROUP_REACTIONS_ENVELOPE_MAX_LEN`]. Any violation refuses the whole
//! envelope, and **`compose_group_reactions` refuses exactly what
//! `decode_group_reactions` refuses**: both run
//! [`AllowedReactions::validate`].
//!
//! ## Order — and the stamp must be re-emitted VERBATIM
//!
//! [`group_reactions_is_newer`]: strictly greater per group wins; equal is
//! not newer. A growth re-send (so a newcomer learns the setting) carries the
//! ORIGINAL stamp, persisted beside the value: the ADR-0044 lesson, as for
//! the group name. Re-stamping at send time would let a stale setting beat a
//! change that landed in between.
//!
//! ## Cross-kind isolation
//!
//! The separator is byte-distinct from, and neither a prefix of nor prefixed
//! by, every sibling envelope domain;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it, since
//! [`crate::incoming_envelope`] dispatches on a byte-prefix match.

use thiserror::Error;

pub use crate::allowed_reactions::AllowedReactions;
use crate::allowed_reactions::{AllowedReactionsError, ALLOWED_REACTIONS_ENCODED_MAX_LEN};

/// 25-byte domain separator prepended to the payload. Frozen: the
/// dispatcher matches on these exact bytes; the version byte is the knob
/// that moves.
pub const GROUP_REACTIONS_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-reactions/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const GROUP_REACTIONS_VERSION_V1: u8 = 0x01;

/// Bytes after the separator before the value: version, `updated_at_ms`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8;

/// Shortest well-formed envelope: an `All` or `Off` value (one mode byte).
pub const GROUP_REACTIONS_ENVELOPE_MIN_LEN: usize =
    GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN + 1;

/// Longest well-formed envelope: a full list of maximal emoji.
pub const GROUP_REACTIONS_ENVELOPE_MAX_LEN: usize = GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len()
    + FIXED_PAYLOAD_LEN
    + ALLOWED_REACTIONS_ENCODED_MAX_LEN;

/// A group's allowed reactions, as an owner or admin set them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReactions {
    /// What the group takes.
    pub reactions: AllowedReactions,
    /// Setter-clock UNIX milliseconds at which this value was set. Orders
    /// values for one group via [`group_reactions_is_newer`]; persisted and
    /// re-emitted verbatim on a growth re-send.
    pub updated_at_ms: u64,
}

/// Why a group allowed-reactions envelope was refused. A reason code; no
/// variant carries an emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupReactionsError {
    /// The plaintext does not carry the domain separator. Only from
    /// [`decode_group_reactions`]; [`try_unwrap_group_reactions`] returns
    /// `Ok(None)`.
    #[error("plaintext does not carry the group-reactions domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("group-reactions envelope is truncated")]
    Truncated,
    /// Bytes remain after the value.
    #[error("group-reactions envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the value.
        count: usize,
    },
    /// The version byte is not [`GROUP_REACTIONS_VERSION_V1`].
    #[error("group-reactions envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The mode byte is none of all, none and list.
    #[error("group-reactions envelope carries an unknown reactions mode {0:#04x}")]
    ReactionsMode(u8),
    /// The list is empty, too long, repeats an emoji or holds a bad entry.
    #[error("invalid allowed-reactions list")]
    Reactions,
}

impl GroupReactionsError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::Truncated => "truncated",
            Self::TrailingBytes { .. } => "trailingBytes",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::ReactionsMode(_) => "reactionsMode",
            Self::Reactions => "reactions",
        }
    }
}

impl From<AllowedReactionsError> for GroupReactionsError {
    fn from(e: AllowedReactionsError) -> Self {
        match e {
            AllowedReactionsError::Truncated => Self::Truncated,
            AllowedReactionsError::Mode(m) => Self::ReactionsMode(m),
            AllowedReactionsError::List => Self::Reactions,
        }
    }
}

/// `true` when a value stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` for the **same group**. Strictly greater
/// wins; equal is not newer.
#[must_use]
pub const fn group_reactions_is_newer(
    incoming_updated_at_ms: u64,
    stored_updated_at_ms: u64,
) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose the envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// # Errors
///
/// Exactly what [`AllowedReactions::validate`] refuses, as
/// [`GroupReactionsError::Reactions`].
pub fn compose_group_reactions(value: &GroupReactions) -> Result<Vec<u8>, GroupReactionsError> {
    let mut out = Vec::with_capacity(GROUP_REACTIONS_ENVELOPE_MIN_LEN);
    out.extend_from_slice(GROUP_REACTIONS_ENVELOPE_DOMAIN_V1);
    out.push(GROUP_REACTIONS_VERSION_V1);
    out.extend_from_slice(&value.updated_at_ms.to_be_bytes());
    value.reactions.encode_into(&mut out)?;
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(_))` — the separator matched and the envelope is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed. The caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// Every [`GroupReactionsError`] but [`GroupReactionsError::WrongDomain`];
/// the version is judged first, then the stamp, the value (its count and
/// lengths before anything is copied), trailing bytes, then the list rule.
pub fn try_unwrap_group_reactions(
    plaintext: &[u8],
) -> Result<Option<GroupReactions>, GroupReactionsError> {
    let Some(mut cur) = plaintext.strip_prefix(GROUP_REACTIONS_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = cur.split_first().ok_or(GroupReactionsError::Truncated)?;
    cur = rest;
    if version != GROUP_REACTIONS_VERSION_V1 {
        return Err(GroupReactionsError::UnknownVersion(version));
    }
    if cur.len() < 8 {
        return Err(GroupReactionsError::Truncated);
    }
    let (stamp, rest) = cur.split_at(8);
    cur = rest;
    let mut stamp_bytes = [0_u8; 8];
    stamp_bytes.copy_from_slice(stamp);
    let updated_at_ms = u64::from_be_bytes(stamp_bytes);
    let reactions = AllowedReactions::decode_from(&mut cur)?;
    if !cur.is_empty() {
        return Err(GroupReactionsError::TrailingBytes { count: cur.len() });
    }
    reactions.validate()?;
    Ok(Some(GroupReactions {
        reactions,
        updated_at_ms,
    }))
}

/// Strict standalone decoder: a plaintext of another kind is an error.
///
/// # Errors
///
/// [`GroupReactionsError::WrongDomain`], and every error
/// [`try_unwrap_group_reactions`] returns.
pub fn decode_group_reactions(bytes: &[u8]) -> Result<GroupReactions, GroupReactionsError> {
    try_unwrap_group_reactions(bytes)?.ok_or(GroupReactionsError::WrongDomain)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUMBS: &str = "\u{1F44D}";
    const HEART: &str = "\u{2764}\u{FE0F}";

    fn value(reactions: AllowedReactions, at: u64) -> GroupReactions {
        GroupReactions {
            reactions,
            updated_at_ms: at,
        }
    }

    fn only(list: &[&str]) -> AllowedReactions {
        AllowedReactions::Only(list.iter().map(|s| (*s).to_string()).collect())
    }

    #[test]
    fn every_mode_round_trips() {
        for v in [
            value(AllowedReactions::All, 0),
            value(AllowedReactions::Off, u64::MAX),
            value(only(&[HEART, THUMBS]), 1_700_000_000_000),
        ] {
            let wire = compose_group_reactions(&v).unwrap();
            assert!(wire.len() >= GROUP_REACTIONS_ENVELOPE_MIN_LEN);
            assert!(wire.len() <= GROUP_REACTIONS_ENVELOPE_MAX_LEN);
            assert_eq!(decode_group_reactions(&wire), Ok(v));
        }
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire = compose_group_reactions(&value(only(&[THUMBS]), 0x0102_0304_0506_0708)).unwrap();
        let sep = GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(sep, 25);
        assert_eq!(&wire[..sep], b"lakko/group-reactions/v1\x00");
        assert_eq!(wire[sep], GROUP_REACTIONS_VERSION_V1);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&wire[sep + 9..sep + 12], &[2, 1, 4]);
        assert_eq!(&wire[sep + 12..], THUMBS.as_bytes());
        assert_eq!(GROUP_REACTIONS_ENVELOPE_MIN_LEN, 35);
        assert_eq!(GROUP_REACTIONS_ENVELOPE_MAX_LEN, 34 + 6502);
        let all = compose_group_reactions(&value(AllowedReactions::All, 0)).unwrap();
        assert_eq!(all.len(), GROUP_REACTIONS_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        let t = THUMBS.to_string();
        for bad in [
            AllowedReactions::Only(vec![]),
            AllowedReactions::Only(vec![t.clone(), t.clone()]),
            AllowedReactions::Only(vec![String::new()]),
            AllowedReactions::Only(vec![t; 101]),
        ] {
            assert_eq!(
                compose_group_reactions(&value(bad, 1)),
                Err(GroupReactionsError::Reactions)
            );
        }
        // A duplicate on the wire, hand-built past compose.
        let mut dup = compose_group_reactions(&value(AllowedReactions::All, 1)).unwrap();
        dup.pop();
        dup.extend_from_slice(&[2, 2, 4]);
        dup.extend_from_slice(THUMBS.as_bytes());
        dup.push(4);
        dup.extend_from_slice(THUMBS.as_bytes());
        assert_eq!(
            decode_group_reactions(&dup),
            Err(GroupReactionsError::Reactions)
        );
    }

    #[test]
    fn decode_is_fail_closed() {
        assert_eq!(try_unwrap_group_reactions(b"hello"), Ok(None));
        assert_eq!(try_unwrap_group_reactions(&[]), Ok(None));
        assert_eq!(
            decode_group_reactions(b"hello"),
            Err(GroupReactionsError::WrongDomain)
        );
        assert_eq!(
            try_unwrap_group_reactions(GROUP_REACTIONS_ENVELOPE_DOMAIN_V1),
            Err(GroupReactionsError::Truncated)
        );
        let wire = compose_group_reactions(&value(only(&[HEART, THUMBS]), 9)).unwrap();
        for cut in GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_group_reactions(&wire[..cut]),
                Err(GroupReactionsError::Truncated),
                "cut at {cut}"
            );
        }
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_group_reactions(&trailing),
            Err(GroupReactionsError::TrailingBytes { count: 1 })
        );
        let sep = GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len();
        for v in [0x00, 0x02, 0xFF] {
            let mut bad = wire.clone();
            bad[sep] = v;
            assert_eq!(
                decode_group_reactions(&bad),
                Err(GroupReactionsError::UnknownVersion(v))
            );
            // The version is judged before everything after it.
            assert_eq!(
                decode_group_reactions(&bad[..=sep]),
                Err(GroupReactionsError::UnknownVersion(v))
            );
        }
        let mut mode = wire;
        mode[sep + 9] = 3;
        assert_eq!(
            decode_group_reactions(&mode),
            Err(GroupReactionsError::ReactionsMode(3))
        );
    }

    #[test]
    fn newer_is_strictly_greater() {
        assert!(group_reactions_is_newer(2, 1));
        assert!(!group_reactions_is_newer(1, 1));
        assert!(!group_reactions_is_newer(0, 1));
    }

    #[test]
    fn a_growth_resend_with_the_original_stamp_never_clobbers_a_change() {
        let original = value(only(&[THUMBS]), 10);
        let changed = value(AllowedReactions::Off, 20);
        let resend = decode_group_reactions(&compose_group_reactions(&original).unwrap()).unwrap();
        assert_eq!(resend.updated_at_ms, 10);
        assert!(!group_reactions_is_newer(
            resend.updated_at_ms,
            changed.updated_at_ms
        ));
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(GroupReactionsError::WrongDomain.code(), "wrongDomain");
        assert_eq!(GroupReactionsError::Truncated.code(), "truncated");
        assert_eq!(
            GroupReactionsError::TrailingBytes { count: 2 }.code(),
            "trailingBytes"
        );
        assert_eq!(
            GroupReactionsError::UnknownVersion(2).code(),
            "unknownVersion"
        );
        assert_eq!(
            GroupReactionsError::ReactionsMode(3).code(),
            "reactionsMode"
        );
        assert_eq!(GroupReactionsError::Reactions.code(), "reactions");
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
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
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
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::link_claim::LINK_CLAIM_ENVELOPE_DOMAIN_V1,
            crate::link_founded::LINK_FOUNDED_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, GROUP_REACTIONS_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(GROUP_REACTIONS_ENVELOPE_DOMAIN_V1)
                    && !GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn the_envelope_dispatches_as_its_own_kind_and_siblings_fall_through() {
        use crate::group_name::{compose_group_name, try_unwrap_group_name, GroupName};
        use crate::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
        use crate::pin::{compose_pin, try_unwrap_pin, PinAction, PinOp};

        let v = value(only(&[HEART]), 5);
        let wire = compose_group_reactions(&v).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire),
            Ok(IncomingEnvelope::GroupReactions(v))
        );
        assert_eq!(try_unwrap_group_name(&wire), Ok(None));
        assert_eq!(try_unwrap_pin(&wire), Ok(None));

        let name = compose_group_name(&GroupName {
            name: "Trio".to_string(),
            updated_at_ms: 1,
        })
        .unwrap();
        let pin = compose_pin(&PinOp {
            message_id: 1,
            action: PinAction::Pin,
            updated_at_ms: 1,
        });
        for sibling in [name, pin] {
            assert_eq!(try_unwrap_group_reactions(&sibling), Ok(None));
        }

        // A matched domain with a malformed payload is an error, never an
        // intro message.
        let mut truncated = GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.to_vec();
        truncated.push(GROUP_REACTIONS_VERSION_V1);
        assert_eq!(
            dispatch_incoming_envelope(&truncated),
            Err(crate::incoming_envelope::DispatchError::GroupReactions(
                GroupReactionsError::Truncated
            ))
        );
    }
}
