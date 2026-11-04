// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_group_photo` (ADR-0087 §18.1).
//!
//! A group-photo envelope arrives as an MLS application message on a group
//! journal — from any member (the role gate that decides whether the sender
//! may set the photo runs AFTER this decode, on the receiver), or from the
//! zero-trust DS replaying a corrupt row — so the bytes reach this bounded,
//! fail-closed decoder before any state is consulted. The parser is the
//! shared avatar field: a length byte admitting 0, 84 or 170, two fixed
//! references and the square / size / duration bounds.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * a decoded envelope is exactly one of the three advertised lengths, and
//!   its avatar is within every bound;
//! * **round-trip stability** — anything that decodes re-composes to exactly
//!   the input bytes (the encoding is canonical) and decodes to the identical
//!   photo;
//! * the order is total and panic-free, and a photo is never newer than
//!   itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::avatar::{
    AVATAR_STILL_MAX_BYTES, AVATAR_STILL_MAX_PX, AVATAR_VIDEO_MAX_BYTES,
    AVATAR_VIDEO_MAX_DURATION_MS, AVATAR_VIDEO_MAX_PX,
};
use lakko_contacts::group_photo::{
    compose_group_photo, decode_group_photo, group_photo_is_newer, GROUP_PHOTO_ENVELOPE_MAX_LEN,
    GROUP_PHOTO_ENVELOPE_MIN_LEN, GROUP_PHOTO_ENVELOPE_STILL_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(photo) = decode_group_photo(data) else {
        return;
    };

    assert!([
        GROUP_PHOTO_ENVELOPE_MIN_LEN,
        GROUP_PHOTO_ENVELOPE_STILL_LEN,
        GROUP_PHOTO_ENVELOPE_MAX_LEN
    ]
    .contains(&data.len()));

    if let Some(avatar) = &photo.avatar {
        assert_eq!(avatar.still.width, avatar.still.height);
        assert!(avatar.still.width >= 1 && avatar.still.width <= AVATAR_STILL_MAX_PX);
        assert!(avatar.still.size >= 1 && avatar.still.size <= AVATAR_STILL_MAX_BYTES);
        if let Some(video) = &avatar.video {
            assert_eq!(video.reference.width, video.reference.height);
            assert!(video.reference.width >= 1 && video.reference.width <= AVATAR_VIDEO_MAX_PX);
            assert!(video.reference.size >= 1 && video.reference.size <= AVATAR_VIDEO_MAX_BYTES);
            assert!(video.duration_ms >= 1 && video.duration_ms <= AVATAR_VIDEO_MAX_DURATION_MS);
        }
    }

    let recomposed = compose_group_photo(&photo).expect("compose must accept what decode accepted");
    assert_eq!(
        recomposed.as_slice(),
        data,
        "the encoding must be canonical"
    );
    let again = decode_group_photo(&recomposed).expect("a composed group photo must decode");
    assert_eq!(again, photo);

    assert!(!group_photo_is_newer(
        photo.updated_at_ms,
        photo.updated_at_ms
    ));
    let _ = group_photo_is_newer(photo.updated_at_ms, u64::MAX);
});
