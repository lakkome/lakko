// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the sealed media frame parser and open path (ADR-0071).
//!
//! A sealed media frame is `clear_prefix || ciphertext_and_tag ||
//! sframe_header || trailer`. A receiver reads the 3-byte trailer at the end
//! of the frame to learn where the clear prefix stops and where the SFrame
//! header starts, on bytes handed over by an untrusted SFU or peer BEFORE the
//! AEAD authenticates anything. The two length bytes, the version byte and
//! the header bytes they point at are all attacker-chosen, so every slice
//! taken from them must be bounds-checked first.
//!
//! The harness asserts, on arbitrary input:
//!   1. `MediaFrameParts::split` never panics;
//!   2. a split that succeeds accounts for every byte of the input, within
//!      the layout's bounds (clear prefix <= 16, ciphertext >= the tag,
//!      header 1..=17, trailer 3), and its regions lie in wire order;
//!   3. `CallKeyRing::try_open_media_frame_with_overlap` under a fixed seed
//!      never panics and never accepts (the fuzzer cannot forge a tag), with
//!      and without a retained previous key;
//!   4. the sender's prefix rule never asks for more than the frame holds or
//!      than the bound, and sealing the input as a frame opens back to it.

#![no_main]

use lakko_calls::media_frame::{
    clear_prefix_len, MediaCodec, MediaFrameParts, MAX_CLEAR_PREFIX_LEN, MEDIA_FRAME_TAG_LEN,
    MEDIA_FRAME_TRAILER_LEN,
};
use lakko_calls::{CallKeyRing, SframeHeader, MAX_HEADER_LEN};
use libfuzzer_sys::fuzz_target;

const SEED: [u8; 32] = [0x42; 32];
const PREVIOUS_SEED: [u8; 32] = [0x24; 32];

fuzz_target!(|data: &[u8]| {
    // (1)+(2) The split.
    if let Ok(parts) = MediaFrameParts::split(data) {
        assert!(parts.clear_prefix.len() <= MAX_CLEAR_PREFIX_LEN);
        assert!(parts.ciphertext.len() >= MEDIA_FRAME_TAG_LEN);
        assert!(!parts.header_bytes.is_empty() && parts.header_bytes.len() <= MAX_HEADER_LEN);
        assert_eq!(parts.trailer.len(), MEDIA_FRAME_TRAILER_LEN);
        let mut rebuilt = Vec::with_capacity(data.len());
        rebuilt.extend_from_slice(parts.clear_prefix);
        rebuilt.extend_from_slice(parts.ciphertext);
        rebuilt.extend_from_slice(parts.header_bytes);
        rebuilt.extend_from_slice(parts.trailer);
        assert_eq!(rebuilt, data, "the regions are not the input in wire order");
        let (header, consumed) =
            SframeHeader::decode(parts.header_bytes).expect("split decoded this header");
        assert_eq!(header, parts.header);
        assert_eq!(consumed, parts.header_bytes.len());
    }

    // (3) The open, with and without an overlap window.
    let mut ring = CallKeyRing::from_seed(PREVIOUS_SEED);
    assert!(ring.try_open_media_frame_with_overlap(data).is_err());
    ring.rekey_from_seed_at(SEED, 0, 1_000);
    assert!(ring.try_open_media_frame_with_overlap(data).is_err());

    // (4) The sender side, with the input as an encoded frame.
    let codec = MediaCodec::from_u8(data.first().copied().unwrap_or(0) >> 6);
    let clear = clear_prefix_len(codec, data);
    assert!(clear <= data.len() && clear <= MAX_CLEAR_PREFIX_LEN);
    let kid = SframeHeader::kid_for(7, 1).expect("stream 1 fits");
    let mut key = ring.participant_key(kid).expect("participant key");
    let wire = key
        .seal_media_frame_for_codec(data, codec)
        .expect("a fresh key seals");
    assert_eq!(&wire[..clear], &data[..clear]);
    let opened = ring
        .try_open_media_frame_with_overlap(&wire)
        .expect("a frame this ring sealed opens");
    assert_eq!(opened, data);
});
