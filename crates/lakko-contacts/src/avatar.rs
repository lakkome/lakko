// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The avatar field — a reference to an encrypted photo (and optionally a
//! short video) in storage, shared by every carrier that names a picture
//! ([ADR-0084](../../../docs/adr/0084-profile-photo.md),
//! [ADR-0087](../../../docs/adr/0087-group-and-channel-handles-and-privacy.md)
//! section 18).
//!
//! One codec, used unchanged by the person's profile ([`crate::profile`]),
//! the group photo envelope ([`crate::group_photo`]), the handle record
//! ([`crate::handle_record`]), the invite-link preview
//! ([`crate::group_invite`]) and the channel metadata record
//! (`lakko-channels::meta`). The bounds therefore cannot drift between a
//! person's photo and a group's.
//!
//! ## Wire shape
//!
//! ```text
//!   field  = avatar_len(1: 0 | 84 | 170) || avatar
//!   avatar = still_ref | still_ref || video_ref || duration_ms(2 B, BE)
//!   ref    = blob_id(32) || key(32) || nonce(12) || size(4 B, BE)
//!            || width(2 B, BE) || height(2 B, BE)
//! ```
//!
//! `avatar_len = 0` means "no photo". Any other length byte is refused.
//!
//! ## Bounds
//!
//! * The still: square, 1..=[`AVATAR_STILL_MAX_PX`] px, 1..=
//!   [`AVATAR_STILL_MAX_BYTES`] bytes.
//! * The video: square, 1..=[`AVATAR_VIDEO_MAX_PX`] px, 1..=
//!   [`AVATAR_VIDEO_MAX_BYTES`] bytes, 1..=[`AVATAR_VIDEO_MAX_DURATION_MS`] ms.
//!
//! A carrier that holds an avatar violating any bound refuses its **whole**
//! envelope: there is no partial acceptance.
//!
//! ## The keys are secret
//!
//! Each reference carries the ChaCha20-Poly1305 key of its blob, so the
//! field must travel only inside something already encrypted to the people
//! meant to see the photo. [`AvatarRef`]'s `Debug` omits the blob id, the
//! key and the nonce.

use core::result::Result as CoreResult;

use thiserror::Error;

/// Length of an encoded [`AvatarRef`]: blob id, key, nonce, size, width,
/// height.
pub const AVATAR_REF_LEN: usize = 32 + 32 + 12 + 4 + 2 + 2;

/// Length of an avatar field holding a still only.
pub const AVATAR_STILL_ONLY_LEN: usize = AVATAR_REF_LEN;

/// Length of an avatar field holding a still, a video and its duration.
pub const AVATAR_WITH_VIDEO_LEN: usize = 2 * AVATAR_REF_LEN + 2;

/// Most bytes an avatar field takes on the wire, its length byte included.
pub const AVATAR_FIELD_MAX_LEN: usize = 1 + AVATAR_WITH_VIDEO_LEN;

/// Largest side of the avatar still, in pixels.
pub const AVATAR_STILL_MAX_PX: u16 = 640;

/// Largest avatar still, in plaintext bytes (256 KiB).
pub const AVATAR_STILL_MAX_BYTES: u32 = 256 * 1024;

/// Largest side of the avatar video, in pixels.
pub const AVATAR_VIDEO_MAX_PX: u16 = 480;

/// Largest avatar video, in plaintext bytes (4 MiB).
pub const AVATAR_VIDEO_MAX_BYTES: u32 = 4 * 1024 * 1024;

/// Longest avatar video, in milliseconds.
pub const AVATAR_VIDEO_MAX_DURATION_MS: u16 = 10_000;

/// A reference to one encrypted avatar blob in storage (ADR-0084): what
/// `lakko_attachments::encrypt_attachment` produced for it, plus its size
/// and pixel dimensions.
#[derive(Clone, PartialEq, Eq)]
pub struct AvatarRef {
    /// `SHA-256` of the ciphertext: the storage address.
    pub blob_id: [u8; 32],
    /// The ChaCha20-Poly1305 key. Secret.
    pub key: [u8; 32],
    /// The AEAD nonce.
    pub nonce: [u8; 12],
    /// Plaintext size in bytes.
    pub size: u32,
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels; equal to `width`.
    pub height: u16,
}

impl core::fmt::Debug for AvatarRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AvatarRef")
            .field("size", &self.size)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A profile video: its blob and its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarVideo {
    /// The encrypted square MP4.
    pub reference: AvatarRef,
    /// Duration in milliseconds, 1..=[`AVATAR_VIDEO_MAX_DURATION_MS`].
    pub duration_ms: u16,
}

/// A photo: a still, and optionally a short video whose first frame the
/// still is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    /// The encrypted square JPEG, shown everywhere.
    pub still: AvatarRef,
    /// The encrypted square MP4, played in large views.
    pub video: Option<AvatarVideo>,
}

/// Why an avatar field was refused.
///
/// A reason code: no variant carries a key or a blob id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AvatarError {
    /// The field ends before its declared length.
    #[error("avatar field is truncated")]
    Truncated,
    /// The length byte is not 0, [`AVATAR_STILL_ONLY_LEN`] or
    /// [`AVATAR_WITH_VIDEO_LEN`].
    #[error("avatar is {got} bytes, expected 0, 84 or 170")]
    Length {
        /// The length byte as read.
        got: usize,
    },
    /// An image or video is not square.
    #[error("avatar is not square")]
    NotSquare,
    /// A side is 0 or above its slot's limit.
    #[error("avatar dimensions are out of bounds")]
    Dimensions,
    /// A blob size is 0 or above its slot's limit.
    #[error("avatar size is out of bounds")]
    Size,
    /// The video duration is 0 or above the limit.
    #[error("avatar video duration is out of bounds")]
    Duration,
}

impl AvatarError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen. A
    /// carrier namespaces it (`avatar.notSquare`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Truncated => "truncated",
            Self::Length { .. } => "length",
            Self::NotSquare => "notSquare",
            Self::Dimensions => "dimensions",
            Self::Size => "size",
            Self::Duration => "duration",
        }
    }
}

impl AvatarRef {
    fn check(&self, max_px: u16, max_bytes: u32) -> CoreResult<(), AvatarError> {
        if self.width != self.height {
            return Err(AvatarError::NotSquare);
        }
        if self.width == 0 || self.width > max_px {
            return Err(AvatarError::Dimensions);
        }
        if self.size == 0 || self.size > max_bytes {
            return Err(AvatarError::Size);
        }
        Ok(())
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.blob_id);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.size.to_be_bytes());
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
    }

    fn decode(cur: &mut &[u8]) -> CoreResult<Self, AvatarError> {
        let blob_id = take_array::<32>(cur)?;
        let key = take_array::<32>(cur)?;
        let nonce = take_array::<12>(cur)?;
        let size = u32::from_be_bytes(take_array::<4>(cur)?);
        let width = u16::from_be_bytes(take_array::<2>(cur)?);
        let height = u16::from_be_bytes(take_array::<2>(cur)?);
        Ok(Self {
            blob_id,
            key,
            nonce,
            size,
            width,
            height,
        })
    }
}

impl Avatar {
    /// Check every avatar bound (ADR-0084).
    ///
    /// # Errors
    ///
    /// [`AvatarError::NotSquare`], [`AvatarError::Dimensions`],
    /// [`AvatarError::Size`], [`AvatarError::Duration`].
    pub fn validate(&self) -> CoreResult<(), AvatarError> {
        self.still
            .check(AVATAR_STILL_MAX_PX, AVATAR_STILL_MAX_BYTES)?;
        if let Some(video) = &self.video {
            video
                .reference
                .check(AVATAR_VIDEO_MAX_PX, AVATAR_VIDEO_MAX_BYTES)?;
            if video.duration_ms == 0 || video.duration_ms > AVATAR_VIDEO_MAX_DURATION_MS {
                return Err(AvatarError::Duration);
            }
        }
        Ok(())
    }

    /// The value of the length byte for this avatar: 84 or 170.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        if self.video.is_some() {
            AVATAR_WITH_VIDEO_LEN
        } else {
            AVATAR_STILL_ONLY_LEN
        }
    }
}

/// Bytes an avatar field takes on the wire, its length byte included: 1,
/// 85 or 171.
#[must_use]
pub const fn avatar_field_len(avatar: Option<&Avatar>) -> usize {
    match avatar {
        Some(a) => 1 + a.encoded_len(),
        None => 1,
    }
}

/// Append the avatar field (`avatar_len || avatar`) to `out`.
///
/// Writes no bound check: a carrier runs [`Avatar::validate`] in its own
/// `validate`, before it composes anything, so compose refuses exactly what
/// decode refuses.
pub fn encode_avatar_field(avatar: Option<&Avatar>, out: &mut Vec<u8>) {
    match avatar {
        Some(avatar) => {
            // 84 or 170, both below 256.
            out.push(u8::try_from(avatar.encoded_len()).unwrap_or(u8::MAX));
            avatar.still.encode_into(out);
            if let Some(video) = &avatar.video {
                video.reference.encode_into(out);
                out.extend_from_slice(&video.duration_ms.to_be_bytes());
            }
        },
        None => out.push(0),
    }
}

/// Read an avatar field from the front of `cur` and advance past it,
/// **without** checking the bounds.
///
/// For a carrier that runs [`Avatar::validate`] in its own `validate` after
/// the whole envelope is parsed (the profile does, so its error precedence
/// stays what it was). Every other carrier uses [`decode_avatar_field`].
///
/// # Errors
///
/// [`AvatarError::Truncated`], [`AvatarError::Length`].
pub fn read_avatar_field(cur: &mut &[u8]) -> CoreResult<Option<Avatar>, AvatarError> {
    let (&len, rest) = cur.split_first().ok_or(AvatarError::Truncated)?;
    *cur = rest;
    let avatar_len = usize::from(len);
    match avatar_len {
        0 => Ok(None),
        AVATAR_STILL_ONLY_LEN | AVATAR_WITH_VIDEO_LEN => {
            let still = AvatarRef::decode(cur)?;
            let video = if avatar_len == AVATAR_WITH_VIDEO_LEN {
                let reference = AvatarRef::decode(cur)?;
                let duration_ms = u16::from_be_bytes(take_array::<2>(cur)?);
                Some(AvatarVideo {
                    reference,
                    duration_ms,
                })
            } else {
                None
            };
            Ok(Some(Avatar { still, video }))
        },
        got => Err(AvatarError::Length { got }),
    }
}

/// Read an avatar field from the front of `cur`, advance past it, and check
/// every bound.
///
/// # Errors
///
/// Every error [`read_avatar_field`] and [`Avatar::validate`] return.
pub fn decode_avatar_field(cur: &mut &[u8]) -> CoreResult<Option<Avatar>, AvatarError> {
    let avatar = read_avatar_field(cur)?;
    if let Some(a) = &avatar {
        a.validate()?;
    }
    Ok(avatar)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> CoreResult<[u8; N], AvatarError> {
    if cur.len() < N {
        return Err(AvatarError::Truncated);
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    head.try_into().map_err(|_| AvatarError::Truncated)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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

    fn field(avatar: Option<&Avatar>) -> Vec<u8> {
        let mut out = Vec::new();
        encode_avatar_field(avatar, &mut out);
        out
    }

    #[test]
    fn lengths_are_the_documented_construction() {
        assert_eq!(AVATAR_REF_LEN, 84);
        assert_eq!(AVATAR_WITH_VIDEO_LEN, 170);
        assert_eq!(AVATAR_FIELD_MAX_LEN, 171);
        assert_eq!(field(None), vec![0]);
        assert_eq!(field(Some(&still())).len(), 85);
        assert_eq!(field(Some(&with_video())).len(), 171);
        assert_eq!(avatar_field_len(None), 1);
        assert_eq!(avatar_field_len(Some(&still())), 85);
        assert_eq!(avatar_field_len(Some(&with_video())), 171);
    }

    #[test]
    fn round_trips_and_advances_exactly_past_the_field() {
        for avatar in [None, Some(still()), Some(with_video())] {
            let mut wire = field(avatar.as_ref());
            wire.extend_from_slice(b"tail");
            let mut cur = wire.as_slice();
            assert_eq!(decode_avatar_field(&mut cur), Ok(avatar));
            assert_eq!(cur, b"tail");
        }
    }

    #[test]
    fn layout_is_pinned() {
        let a = Avatar {
            still: AvatarRef {
                blob_id: [1; 32],
                key: [2; 32],
                nonce: [3; 12],
                size: 0x0102_0304,
                width: 0x0280,
                height: 0x0280,
            },
            video: None,
        };
        let wire = field(Some(&a));
        assert_eq!(wire[0], 84);
        assert_eq!(&wire[1..33], &[1; 32]);
        assert_eq!(&wire[33..65], &[2; 32]);
        assert_eq!(&wire[65..77], &[3; 12]);
        assert_eq!(&wire[77..81], &[1, 2, 3, 4]);
        assert_eq!(&wire[81..85], &[2, 0x80, 2, 0x80]);
    }

    #[test]
    fn other_length_bytes_are_refused() {
        for got in [1_u8, 83, 85, 169, 171, 255] {
            let mut wire = vec![got];
            wire.resize(1 + usize::from(got), 0);
            let mut cur = wire.as_slice();
            assert_eq!(
                read_avatar_field(&mut cur),
                Err(AvatarError::Length {
                    got: usize::from(got)
                })
            );
        }
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = field(Some(&with_video()));
        for cut in 0..wire.len() {
            let mut cur = &wire[..cut];
            assert_eq!(
                read_avatar_field(&mut cur),
                Err(AvatarError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn every_bound_is_refused_by_decode_and_validate() {
        let mut too_big = still();
        too_big.still.size = AVATAR_STILL_MAX_BYTES + 1;
        let mut zero_size = still();
        zero_size.still.size = 0;
        let mut not_square = still();
        not_square.still.height = 639;
        let mut too_wide = still();
        too_wide.still.width = 641;
        too_wide.still.height = 641;
        let mut zero_px = still();
        zero_px.still.width = 0;
        zero_px.still.height = 0;
        let mut video_too_wide = with_video();
        if let Some(v) = &mut video_too_wide.video {
            v.reference.width = 481;
            v.reference.height = 481;
        }
        let mut video_too_big = with_video();
        if let Some(v) = &mut video_too_big.video {
            v.reference.size = AVATAR_VIDEO_MAX_BYTES + 1;
        }
        let mut too_long = with_video();
        if let Some(v) = &mut too_long.video {
            v.duration_ms = AVATAR_VIDEO_MAX_DURATION_MS + 1;
        }
        let mut zero_long = with_video();
        if let Some(v) = &mut zero_long.video {
            v.duration_ms = 0;
        }
        let cases = [
            (too_big, AvatarError::Size),
            (zero_size, AvatarError::Size),
            (not_square, AvatarError::NotSquare),
            (too_wide, AvatarError::Dimensions),
            (zero_px, AvatarError::Dimensions),
            (video_too_wide, AvatarError::Dimensions),
            (video_too_big, AvatarError::Size),
            (too_long, AvatarError::Duration),
            (zero_long, AvatarError::Duration),
        ];
        for (avatar, why) in cases {
            assert_eq!(avatar.validate(), Err(why));
            let wire = field(Some(&avatar));
            let mut cur = wire.as_slice();
            assert_eq!(decode_avatar_field(&mut cur), Err(why));
            // The structural reader alone accepts it: bounds are validate's.
            let mut cur = wire.as_slice();
            assert_eq!(read_avatar_field(&mut cur), Ok(Some(avatar)));
        }
    }

    #[test]
    fn debug_omits_the_key_and_the_blob_id() {
        let text = format!("{:?}", with_video());
        assert!(!text.contains("blob_id"));
        assert!(!text.contains("key"));
        assert!(!text.contains("nonce"));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(AvatarError::Truncated.code(), "truncated");
        assert_eq!(AvatarError::Length { got: 3 }.code(), "length");
        assert_eq!(AvatarError::NotSquare.code(), "notSquare");
        assert_eq!(AvatarError::Dimensions.code(), "dimensions");
        assert_eq!(AvatarError::Size.code(), "size");
        assert_eq!(AvatarError::Duration.code(), "duration");
    }
}
