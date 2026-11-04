// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-photo envelope — the photo of a group, carried end-to-end
//! encrypted to every member
//! ([ADR-0087](../../../docs/adr/0087-group-and-channel-handles-and-privacy.md)
//! section 18.1).
//!
//! ## What travels, and where
//!
//! A group photo is `{avatar, updated_at_ms}`: a reference to an encrypted
//! still (and optionally a short video) in storage, in the
//! [`crate::avatar`] codec a person's profile uses, and the setter's stamp.
//! It rides as an MLS application message on the group's own journal,
//! classified by [`crate::incoming_envelope`] so it is never rendered. The
//! server sees only ciphertext under the group's epoch secret (invariants
//! #1, #2).
//!
//! It follows the group name ([`crate::group_name`], ADR-0051) in
//! everything but its content, and is a separate envelope on purpose: a
//! name and a photo are changed independently, often by different admins,
//! and one record with one stamp would let a photo change undo a rename made
//! a moment before it (ADR-0087 section 18.1).
//!
//! ## What the envelope does NOT establish
//!
//! * **Authority is the RECEIVER's check, through the role table.** The
//!   receiver resolves the MLS sender to the account key at receipt (C-68)
//!   and reads the live role table: owner or admin, accepted if newer;
//!   anyone else, an unresolvable sender or an unreadable table, dropped. A
//!   one-to-one drops it. The envelope carries no identity and no role of
//!   its own.
//! * The whole read-decide-write runs under the per-group lock (C-62
//!   sub-invariant 5).
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       GROUP_PHOTO_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || avatar_len || avatar
//!      └─────── 21 bytes, ASCII ──────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └ 1 B: 0|84|170 ┘
//!
//!                     "lakko/group-photo/v1\x00"
//! ```
//!
//! Exactly [`GROUP_PHOTO_ENVELOPE_MIN_LEN`] (31, no photo),
//! [`GROUP_PHOTO_ENVELOPE_STILL_LEN`] (115, a still) or
//! [`GROUP_PHOTO_ENVELOPE_MAX_LEN`] (201, a still and a video) bytes.
//! `avatar_len = 0` removes the photo.
//!
//! ## Bounds
//!
//! The avatar bounds are [`crate::avatar`]'s, unchanged: square, sizes,
//! duration. An envelope violating any bound is **rejected as a whole**, and
//! **`compose_group_photo` refuses exactly what `decode_group_photo`
//! refuses**, since both run [`GroupPhoto::validate`].
//!
//! ## Order — and the stamp must be re-emitted VERBATIM
//!
//! [`group_photo_is_newer`]: a receiver keeps the photo with the greater
//! `updated_at_ms` per group; equal is not newer. A growth re-send carries
//! the ORIGINAL stamp, persisted beside the photo (the ADR-0044 lesson, as
//! for the group name). Persisting and replaying it is the caller's
//! obligation.
//!
//! ## The keys are secret
//!
//! The avatar carries the blob keys; anyone holding the envelope can fetch
//! and open the photo. [`GroupPhoto`]'s `Debug` prints sizes only.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::avatar::{
    decode_avatar_field, encode_avatar_field, Avatar, AvatarError, AVATAR_STILL_ONLY_LEN,
    AVATAR_WITH_VIDEO_LEN,
};

/// 21-byte domain separator prepended to the group-photo payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with. The printable prefix is `lakko/group-photo/v1`.
///
/// **The `v1` names the envelope FAMILY, not the schema version.** These
/// bytes are frozen: [`crate::incoming_envelope`] dispatches on this exact
/// prefix. The in-payload version byte is the knob that moves.
pub const GROUP_PHOTO_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-photo/v1\x00";

/// Schema-version byte carried first after the separator. Today the sole
/// accepted value.
pub const GROUP_PHOTO_VERSION_V1: u8 = 0x01;

/// Bytes after the separator that every envelope carries: version,
/// `updated_at_ms`, `avatar_len`.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 1;

/// Length of an envelope that removes the photo.
pub const GROUP_PHOTO_ENVELOPE_MIN_LEN: usize =
    GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Length of an envelope carrying a still only.
pub const GROUP_PHOTO_ENVELOPE_STILL_LEN: usize =
    GROUP_PHOTO_ENVELOPE_MIN_LEN + AVATAR_STILL_ONLY_LEN;

/// Length of an envelope carrying a still and a video.
pub const GROUP_PHOTO_ENVELOPE_MAX_LEN: usize =
    GROUP_PHOTO_ENVELOPE_MIN_LEN + AVATAR_WITH_VIDEO_LEN;

/// A group's photo, as set by an owner or admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPhoto {
    /// The photo, or `None` to remove it. Its keys are secret.
    pub avatar: Option<Avatar>,
    /// Setter-clock UNIX milliseconds at which this photo was set. Orders
    /// photos for the same group via [`group_photo_is_newer`]. Persisted and
    /// re-emitted verbatim on a growth re-send.
    pub updated_at_ms: u64,
}

impl GroupPhoto {
    /// Check every bound. The single rule both [`compose_group_photo`] and
    /// [`decode_group_photo`] run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// [`GroupPhotoError::Avatar`] — the avatar violates a bound.
    pub fn validate(&self) -> CoreResult<(), GroupPhotoError> {
        if let Some(avatar) = &self.avatar {
            avatar.validate().map_err(GroupPhotoError::Avatar)?;
        }
        Ok(())
    }
}

/// `true` when a photo stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` for the **same group**.
///
/// Strictly greater wins; **equal is not newer**.
#[must_use]
pub const fn group_photo_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a group-photo envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// # Errors
///
/// Exactly the errors [`GroupPhoto::validate`] returns — `compose` refuses
/// what [`decode_group_photo`] would refuse.
pub fn compose_group_photo(photo: &GroupPhoto) -> CoreResult<Vec<u8>, GroupPhotoError> {
    photo.validate()?;
    let mut out = Vec::with_capacity(GROUP_PHOTO_ENVELOPE_MAX_LEN);
    out.extend_from_slice(GROUP_PHOTO_ENVELOPE_DOMAIN_V1);
    out.push(GROUP_PHOTO_VERSION_V1);
    out.extend_from_slice(&photo.updated_at_ms.to_be_bytes());
    encode_avatar_field(photo.avatar.as_ref(), &mut out);
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(photo))` — the domain separator matched and the envelope is
///   well-formed and within every bound.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the envelope is malformed or out of
///   bounds. The caller drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`GroupPhotoError::Truncated`] — the payload ends before a declared
///   field.
/// * [`GroupPhotoError::UnknownVersion`] — the version byte is not
///   [`GROUP_PHOTO_VERSION_V1`].
/// * [`GroupPhotoError::TrailingBytes`] — bytes remain after the avatar.
/// * [`GroupPhotoError::Avatar`] — the avatar length byte is not 0, 84 or
///   170, or the avatar violates a bound.
pub fn try_unwrap_group_photo(plaintext: &[u8]) -> CoreResult<Option<GroupPhoto>, GroupPhotoError> {
    let Some(payload) = plaintext.strip_prefix(GROUP_PHOTO_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(GroupPhotoError::Truncated)?;
    if version != GROUP_PHOTO_VERSION_V1 {
        return Err(GroupPhotoError::UnknownVersion(version));
    }
    if rest.len() < 8 {
        return Err(GroupPhotoError::Truncated);
    }
    let (stamp, mut cur) = rest.split_at(8);
    let updated_at_ms =
        u64::from_be_bytes(stamp.try_into().map_err(|_| GroupPhotoError::Truncated)?);
    let avatar = decode_avatar_field(&mut cur).map_err(GroupPhotoError::from)?;
    if !cur.is_empty() {
        return Err(GroupPhotoError::TrailingBytes { count: cur.len() });
    }
    Ok(Some(GroupPhoto {
        avatar,
        updated_at_ms,
    }))
}

/// Strict standalone decoder: a non-group-photo plaintext is a hard error.
///
/// # Errors
///
/// * [`GroupPhotoError::WrongDomain`] — `bytes` does not start with
///   [`GROUP_PHOTO_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_group_photo`] returns.
pub fn decode_group_photo(bytes: &[u8]) -> CoreResult<GroupPhoto, GroupPhotoError> {
    try_unwrap_group_photo(bytes)?.ok_or(GroupPhotoError::WrongDomain)
}

/// Why a group-photo envelope was refused.
///
/// A reason code: no variant carries a key or a blob id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GroupPhotoError {
    /// The plaintext does not carry the group-photo domain separator. Only
    /// from [`decode_group_photo`]; [`try_unwrap_group_photo`] returns
    /// `Ok(None)`.
    #[error("plaintext does not carry the group-photo domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("group-photo envelope is truncated")]
    Truncated,
    /// Bytes remain after the avatar field.
    #[error("group-photo envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the avatar.
        count: usize,
    },
    /// The version byte is not [`GROUP_PHOTO_VERSION_V1`].
    #[error("group-photo envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The avatar field is malformed or violates a bound.
    #[error("invalid group photo: {0}")]
    Avatar(AvatarError),
}

impl From<AvatarError> for GroupPhotoError {
    fn from(e: AvatarError) -> Self {
        match e {
            AvatarError::Truncated => Self::Truncated,
            other => Self::Avatar(other),
        }
    }
}

impl GroupPhotoError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::Avatar(e) => format!("avatar.{}", e.code()),
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar::{AvatarRef, AvatarVideo, AVATAR_STILL_MAX_BYTES};

    fn reference(byte: u8, side: u16, bytes: u32) -> AvatarRef {
        AvatarRef {
            blob_id: [byte; 32],
            key: [byte.wrapping_add(1); 32],
            nonce: [byte.wrapping_add(2); 12],
            size: bytes,
            width: side,
            height: side,
        }
    }

    fn still() -> Avatar {
        Avatar {
            still: reference(0x10, 640, 40_000),
            video: None,
        }
    }

    fn with_video() -> Avatar {
        Avatar {
            still: reference(0x20, 480, 30_000),
            video: Some(AvatarVideo {
                reference: reference(0x30, 480, 1_500_000),
                duration_ms: 9_000,
            }),
        }
    }

    fn photo(avatar: Option<Avatar>, at: u64) -> GroupPhoto {
        GroupPhoto {
            avatar,
            updated_at_ms: at,
        }
    }

    // --- wire format ---------------------------------------------------------

    #[test]
    fn compose_then_decode_round_trips() {
        for p in [
            photo(None, 0),
            photo(Some(still()), 1_700_000_000_000),
            photo(Some(with_video()), u64::MAX),
        ] {
            let wire = compose_group_photo(&p).unwrap();
            assert_eq!(decode_group_photo(&wire).unwrap(), p);
        }
    }

    #[test]
    fn envelope_lengths_are_the_documented_construction() {
        assert_eq!(GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len(), 21);
        assert_eq!(GROUP_PHOTO_ENVELOPE_MIN_LEN, 31);
        assert_eq!(GROUP_PHOTO_ENVELOPE_STILL_LEN, 115);
        assert_eq!(GROUP_PHOTO_ENVELOPE_MAX_LEN, 201);
        assert_eq!(compose_group_photo(&photo(None, 1)).unwrap().len(), 31);
        assert_eq!(
            compose_group_photo(&photo(Some(still()), 1)).unwrap().len(),
            115
        );
        assert_eq!(
            compose_group_photo(&photo(Some(with_video()), 1))
                .unwrap()
                .len(),
            201
        );
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire = compose_group_photo(&photo(None, 0x0102_0304_0506_0708)).unwrap();
        let sep = GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(&wire[..sep], b"lakko/group-photo/v1\x00");
        assert_eq!(wire[sep], GROUP_PHOTO_VERSION_V1);
        assert_eq!(&wire[sep + 1..sep + 9], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(wire[sep + 9], 0);
        let wire = compose_group_photo(&photo(Some(still()), 0)).unwrap();
        assert_eq!(wire[sep + 9], 84);
        assert_eq!(&wire[sep + 10..sep + 42], &[0x10; 32]);
    }

    #[test]
    fn removing_the_photo_round_trips() {
        let wire = compose_group_photo(&photo(None, 42)).unwrap();
        assert_eq!(decode_group_photo(&wire), Ok(photo(None, 42)));
    }

    // --- fail-closed decode --------------------------------------------------

    #[test]
    fn non_group_photo_plaintext_falls_through() {
        assert_eq!(try_unwrap_group_photo(b"hi alice"), Ok(None));
        assert_eq!(try_unwrap_group_photo(&[]), Ok(None));
        assert_eq!(
            decode_group_photo(b"hi alice"),
            Err(GroupPhotoError::WrongDomain)
        );
    }

    #[test]
    fn bare_separator_is_truncated_not_a_fall_through() {
        assert_eq!(
            try_unwrap_group_photo(GROUP_PHOTO_ENVELOPE_DOMAIN_V1),
            Err(GroupPhotoError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_group_photo(&photo(Some(with_video()), 9)).unwrap();
        for cut in GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_group_photo(&wire[..cut]),
                Err(GroupPhotoError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        for avatar in [None, Some(still()), Some(with_video())] {
            let mut wire = compose_group_photo(&photo(avatar, 9)).unwrap();
            wire.push(0);
            assert_eq!(
                decode_group_photo(&wire),
                Err(GroupPhotoError::TrailingBytes { count: 1 })
            );
        }
    }

    #[test]
    fn unknown_versions_are_refused() {
        let mut wire = compose_group_photo(&photo(None, 0)).unwrap();
        for v in [0x00, 0x02, 0xFF] {
            wire[GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len()] = v;
            assert_eq!(
                decode_group_photo(&wire),
                Err(GroupPhotoError::UnknownVersion(v))
            );
        }
    }

    #[test]
    fn other_avatar_lengths_are_refused() {
        let mut wire = compose_group_photo(&photo(Some(still()), 0)).unwrap();
        let at = GROUP_PHOTO_ENVELOPE_DOMAIN_V1.len() + 9;
        wire[at] = 83;
        assert_eq!(
            decode_group_photo(&wire),
            Err(GroupPhotoError::Avatar(AvatarError::Length { got: 83 }))
        );
    }

    #[test]
    fn every_bound_is_refused_on_both_compose_and_decode() {
        let mut too_big = still();
        too_big.still.size = AVATAR_STILL_MAX_BYTES + 1;
        let mut not_square = still();
        not_square.still.height = 600;
        let mut too_long = with_video();
        if let Some(v) = &mut too_long.video {
            v.duration_ms = 10_001;
        }
        for (avatar, why) in [
            (too_big, AvatarError::Size),
            (not_square, AvatarError::NotSquare),
            (too_long, AvatarError::Duration),
        ] {
            let p = photo(Some(avatar.clone()), 0);
            assert_eq!(compose_group_photo(&p), Err(GroupPhotoError::Avatar(why)));
            // The same bytes on the wire, hand-built past compose.
            let mut wire = GROUP_PHOTO_ENVELOPE_DOMAIN_V1.to_vec();
            wire.push(GROUP_PHOTO_VERSION_V1);
            wire.extend_from_slice(&0_u64.to_be_bytes());
            encode_avatar_field(Some(&avatar), &mut wire);
            assert_eq!(decode_group_photo(&wire), Err(GroupPhotoError::Avatar(why)));
        }
    }

    #[test]
    fn debug_never_prints_a_key() {
        let p = photo(Some(with_video()), 1);
        let text = format!("{p:?}");
        assert!(!text.contains("key"));
        assert!(!text.contains("blob_id"));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(GroupPhotoError::WrongDomain.code(), "wrongDomain");
        assert_eq!(GroupPhotoError::Truncated.code(), "truncated");
        assert_eq!(
            GroupPhotoError::TrailingBytes { count: 3 }.code(),
            "trailingBytes"
        );
        assert_eq!(GroupPhotoError::UnknownVersion(9).code(), "unknownVersion");
        assert_eq!(
            GroupPhotoError::Avatar(AvatarError::NotSquare).code(),
            "avatar.notSquare"
        );
        assert_eq!(
            GroupPhotoError::Avatar(AvatarError::Length { got: 1 }).code(),
            "avatar.length"
        );
    }

    // --- order ---------------------------------------------------------------

    #[test]
    fn newer_is_strictly_greater() {
        assert!(group_photo_is_newer(2, 1));
        assert!(!group_photo_is_newer(1, 1), "equal is not newer");
        assert!(!group_photo_is_newer(0, 1));
        assert!(!group_photo_is_newer(u64::MAX, u64::MAX));
    }

    #[test]
    fn a_growth_resend_with_the_original_stamp_never_clobbers_a_change() {
        let original = photo(Some(still()), 10);
        let changed = photo(None, 20);
        let resend = decode_group_photo(&compose_group_photo(&original).unwrap()).unwrap();
        assert_eq!(resend.updated_at_ms, 10);
        assert!(!group_photo_is_newer(
            resend.updated_at_ms,
            changed.updated_at_ms
        ));
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
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, GROUP_PHOTO_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(GROUP_PHOTO_ENVELOPE_DOMAIN_V1)
                    && !GROUP_PHOTO_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn group_photo_does_not_cross_dispatch_as_a_sibling() {
        use crate::group_name::{compose_group_name, try_unwrap_group_name, GroupName};
        use crate::profile::try_unwrap_profile;

        let wire = compose_group_photo(&photo(Some(still()), 1)).unwrap();
        assert!(try_unwrap_group_name(&wire).unwrap().is_none());
        assert!(try_unwrap_profile(&wire).unwrap().is_none());

        let name = compose_group_name(&GroupName {
            name: "Trio".to_string(),
            updated_at_ms: 1,
        })
        .unwrap();
        assert_eq!(try_unwrap_group_photo(&name), Ok(None));
    }
}
