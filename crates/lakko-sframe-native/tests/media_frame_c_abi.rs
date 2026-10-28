// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The ADR-0071 sealed media frame through the `extern "C"` boundary: the
//! entry points a native bridge moves to in phase 2. Pins the same golden
//! frame as `lakko_calls::media_frame` and the wasm surface.

#![allow(unsafe_code)]

use lakko_sframe_native::{
    lakko_sframe_buffer_free, lakko_sframe_call_key_ring_free, lakko_sframe_call_key_ring_new,
    lakko_sframe_call_key_ring_open_framed_with_overlap,
    lakko_sframe_call_key_ring_open_media_frame_with_overlap,
    lakko_sframe_call_key_ring_participant_key, lakko_sframe_media_clear_prefix_len,
    lakko_sframe_participant_key_free, lakko_sframe_seal_framed, lakko_sframe_seal_media_frame,
    lakko_sframe_seal_media_frame_for_codec, CallKeyRing, LakkoSframeStatus, ParticipantKey,
    LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN, LAKKO_SFRAME_MEDIA_CODEC_OPUS,
    LAKKO_SFRAME_MEDIA_CODEC_OTHER, LAKKO_SFRAME_MEDIA_CODEC_RED_OPUS,
    LAKKO_SFRAME_MEDIA_CODEC_VP8,
};

const SEED: [u8; 32] = [0x42; 32];
const LEAF: u32 = 3;
const CAMERA: u8 = 1;
const GOLDEN_MEDIA_FRAME_HEX: &str =
    "1002009d012a8002e0012dc56b47f2bfd3ebf051d8893d6d2b2b355f29f3980b60\
     e19c2555c9ad3a6ca9e95f08b228768c0f59a6509103010a0301";

fn golden() -> Vec<u8> {
    (0..GOLDEN_MEDIA_FRAME_HEX.len() / 2)
        .map(|i| {
            u8::from_str_radix(&GOLDEN_MEDIA_FRAME_HEX[i * 2..i * 2 + 2], 16).expect("hex byte")
        })
        .collect()
}

fn vp8_key_frame() -> Vec<u8> {
    let mut frame = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01];
    frame.extend_from_slice(b"lakko media frame golden v1");
    frame
}

fn ring() -> *mut CallKeyRing {
    let mut ring: *mut CallKeyRing = std::ptr::null_mut();
    let status = unsafe { lakko_sframe_call_key_ring_new(SEED.as_ptr(), SEED.len(), &mut ring) };
    assert_eq!(status, LakkoSframeStatus::Ok);
    ring
}

fn sender(ring: *const CallKeyRing) -> *mut ParticipantKey {
    let mut pk: *mut ParticipantKey = std::ptr::null_mut();
    let status = unsafe { lakko_sframe_call_key_ring_participant_key(ring, LEAF, CAMERA, &mut pk) };
    assert_eq!(status, LakkoSframeStatus::Ok);
    pk
}

fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    unsafe { lakko_sframe_buffer_free(ptr, len) };
    out
}

fn open(ring: *const CallKeyRing, wire: &[u8]) -> Result<Vec<u8>, LakkoSframeStatus> {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        lakko_sframe_call_key_ring_open_media_frame_with_overlap(
            ring,
            wire.as_ptr(),
            wire.len(),
            &mut ptr,
            &mut len,
        )
    };
    if status == LakkoSframeStatus::Ok {
        Ok(take(ptr, len))
    } else {
        assert!(ptr.is_null(), "out-pointer untouched on failure");
        Err(status)
    }
}

fn seal(pk: *mut ParticipantKey, frame: &[u8], clear: usize) -> Result<Vec<u8>, LakkoSframeStatus> {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        lakko_sframe_seal_media_frame(pk, frame.as_ptr(), frame.len(), clear, &mut ptr, &mut len)
    };
    if status == LakkoSframeStatus::Ok {
        Ok(take(ptr, len))
    } else {
        Err(status)
    }
}

fn prefix(codec: u8, frame: &[u8]) -> usize {
    unsafe { lakko_sframe_media_clear_prefix_len(codec, frame.as_ptr(), frame.len()) }
}

#[test]
fn golden_media_frame_opens_via_c_abi() {
    let ring = ring();
    assert_eq!(open(ring, &golden()), Ok(vp8_key_frame()));
    unsafe { lakko_sframe_call_key_ring_free(ring) };
}

#[test]
fn seal_keeps_the_codec_header_and_round_trips_via_c_abi() {
    let ring = ring();
    let pk = sender(ring);
    let key_frame = vp8_key_frame();
    let delta_frame = [&[0x11u8, 0x02, 0x00][..], b"delta"].concat();

    assert_eq!(prefix(LAKKO_SFRAME_MEDIA_CODEC_VP8, &key_frame), 10);
    assert_eq!(prefix(LAKKO_SFRAME_MEDIA_CODEC_VP8, &delta_frame), 3);
    assert_eq!(prefix(LAKKO_SFRAME_MEDIA_CODEC_OPUS, &key_frame), 0);
    assert_eq!(prefix(LAKKO_SFRAME_MEDIA_CODEC_OTHER, &key_frame), 0);
    assert_eq!(
        prefix(200, &key_frame),
        0,
        "an unknown codec value is OTHER"
    );
    assert_eq!(prefix(LAKKO_SFRAME_MEDIA_CODEC_VP8, &[0x10, 0x02]), 2);
    assert_eq!(
        unsafe {
            lakko_sframe_media_clear_prefix_len(LAKKO_SFRAME_MEDIA_CODEC_VP8, std::ptr::null(), 0)
        },
        0
    );

    let key_wire = seal(pk, &key_frame, 10).expect("seal key frame");
    let delta_wire = seal(pk, &delta_frame, 3).expect("seal delta frame");
    assert_eq!(&key_wire[..10], &key_frame[..10]);
    assert_eq!(&delta_wire[..3], &delta_frame[..3]);
    assert_eq!(open(ring, &key_wire), Ok(key_frame.clone()));
    assert_eq!(open(ring, &delta_wire), Ok(delta_frame));

    // A tampered clear byte, and a tampered trailer, are refused.
    let mut bad = key_wire.clone();
    bad[0] |= 0x01;
    assert_eq!(open(ring, &bad), Err(LakkoSframeStatus::AeadFailed));
    let mut bad = key_wire.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    assert_eq!(open(ring, &bad), Err(LakkoSframeStatus::WireParse));

    // More than the bound is refused and seals nothing.
    assert_eq!(
        seal(pk, &key_frame, LAKKO_SFRAME_MAX_CLEAR_PREFIX_LEN + 1),
        Err(LakkoSframeStatus::BadClearPrefix)
    );

    unsafe {
        lakko_sframe_participant_key_free(pk);
        lakko_sframe_call_key_ring_free(ring);
    }
}

/// What holds between a web client on ADR-0071 and a native bridge still on
/// the header-first functions: neither opens the other's frames.
#[test]
fn the_two_layouts_do_not_open_as_each_other_via_c_abi() {
    let ring = ring();
    let pk = sender(ring);
    let frame = vp8_key_frame();

    // Header-first frame into the new open.
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    let status =
        unsafe { lakko_sframe_seal_framed(pk, frame.as_ptr(), frame.len(), &mut ptr, &mut len) };
    assert_eq!(status, LakkoSframeStatus::Ok);
    let old = take(ptr, len);
    assert!(open(ring, &old).is_err());

    // New frame into the header-first open.
    let new = seal(pk, &frame, 10).expect("seal");
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        lakko_sframe_call_key_ring_open_framed_with_overlap(
            ring,
            new.as_ptr(),
            new.len(),
            &mut ptr,
            &mut len,
        )
    };
    assert_ne!(status, LakkoSframeStatus::Ok);
    assert!(ptr.is_null());

    unsafe {
        lakko_sframe_participant_key_free(pk);
        lakko_sframe_call_key_ring_free(ring);
    }
}

fn seal_for(pk: *mut ParticipantKey, frame: &[u8], codec: u8) -> Vec<u8> {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0usize;
    let status = unsafe {
        lakko_sframe_seal_media_frame_for_codec(
            pk,
            frame.as_ptr(),
            frame.len(),
            codec,
            &mut ptr,
            &mut len,
        )
    };
    assert_eq!(status, LakkoSframeStatus::Ok);
    take(ptr, len)
}

/// A native sender whose audio is a RED container (`audio/red`) seals it
/// with the trailer saying so; every other codec seals as the codec's own
/// frame, with the prefix the codec's rule gives.
#[test]
fn seal_for_codec_marks_red_in_the_trailer() {
    let ring = ring();
    let pk = sender(ring);
    let mut container = vec![0xef, 0x0f, 0x00, 0x02, 0x6f, 0xaa, 0xbb];
    container.extend_from_slice(b"primary");
    let red = seal_for(pk, &container, LAKKO_SFRAME_MEDIA_CODEC_RED_OPUS);
    let parts = lakko_calls::MediaFrameParts::split(&red).expect("split");
    assert_eq!(parts.content, lakko_calls::MediaContent::RedOpus);
    assert!(parts.clear_prefix.is_empty());
    assert_eq!(open(ring, &red), Ok(container.clone()));

    let opus = seal_for(pk, &container, LAKKO_SFRAME_MEDIA_CODEC_OPUS);
    let parts = lakko_calls::MediaFrameParts::split(&opus).expect("split");
    assert_eq!(parts.content, lakko_calls::MediaContent::Codec);

    let key = seal_for(pk, &vp8_key_frame(), LAKKO_SFRAME_MEDIA_CODEC_VP8);
    assert_eq!(&key[..10], &vp8_key_frame()[..10]);
    assert_eq!(open(ring, &key), Ok(vp8_key_frame()));
    unsafe {
        lakko_sframe_participant_key_free(pk);
        lakko_sframe_call_key_ring_free(ring);
    }
}
