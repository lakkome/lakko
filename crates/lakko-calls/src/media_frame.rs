// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The sealed media frame layout of ADR-0071: the codec's own frame header
//! stays in the clear at the start of the frame, and the SFrame header moves
//! to the end.
//!
//! ```text
//!   clear_prefix || ciphertext_and_tag || sframe_header || trailer
//!   0..=16 B        >= 16 B               1..=17 B         3 B
//!
//!   trailer = clear_prefix_len (1) || sframe_header_len (1) || version (1) = 0x01
//! ```
//!
//! - `clear_prefix` is the first bytes of the encoded frame exactly as the
//!   encoder produced them. A receiver whose RTP stack has no generic frame
//!   descriptor finds a key frame by reading the codec's header at the start
//!   of the payload; with the SFrame header there instead (the ADR-0013
//!   layout) no frame ever looked like a key frame to it.
//! - `ciphertext_and_tag` is the ChaCha20-Poly1305 seal of the rest of the
//!   frame, under the key, nonce and counter rules of ADR-0035 and ADR-0069,
//!   which this layout does not change.
//! - `sframe_header` is the RFC 9605 section 4.3 header ([`SframeHeader`]),
//!   byte for byte the codec of [`crate::header`].
//! - `trailer` is fixed-size and lets a receiver split the frame without
//!   knowing the codec.
//!
//! The AEAD additional data is `sframe_header || clear_prefix || trailer`, so
//! every byte that is not ciphertext is authenticated: an SFU cannot change a
//! clear byte, the header or the trailer without the open failing.
//!
//! This module owns the layout (the split, the trailer, the AAD) and the
//! sender's choice of prefix length per codec. The seal and the open live on
//! [`crate::sframe::ParticipantKey`] and [`crate::CallKeyRing`], which hold
//! the keys.

use thiserror::Error;

use crate::header::{HeaderError, SframeHeader, MAX_HEADER_LEN};

/// Version byte of the trailer, the last byte of every sealed media frame.
pub const MEDIA_FRAME_VERSION: u8 = 0x01;

/// Length of the fixed trailer: clear-prefix length, SFrame header length,
/// version.
pub const MEDIA_FRAME_TRAILER_LEN: usize = 3;

/// The most bytes a sealed media frame may leave in the clear. VP8 needs 10;
/// the bound is a power of two above that, chosen and not measured. A sender
/// asking for more is refused and a receiver refuses a trailer claiming more,
/// so a defect on either side cannot expose more than this of a frame.
pub const MAX_CLEAR_PREFIX_LEN: usize = 16;

/// Length of the Poly1305 tag that ends the ciphertext.
pub const MEDIA_FRAME_TAG_LEN: usize = 16;

/// The shortest well-formed sealed media frame: an empty clear prefix, the
/// seal of an empty plaintext (the tag alone), a one-byte SFrame header and
/// the trailer.
pub const MIN_MEDIA_FRAME_LEN: usize = MEDIA_FRAME_TAG_LEN + 1 + MEDIA_FRAME_TRAILER_LEN;

/// Bytes a sealed media frame adds to a frame at most: the tag, the longest
/// SFrame header and the trailer.
pub const MAX_MEDIA_FRAME_OVERHEAD: usize =
    MEDIA_FRAME_TAG_LEN + MAX_HEADER_LEN + MEDIA_FRAME_TRAILER_LEN;

/// Clear prefix of a VP8 key frame: the 3-byte frame tag, the 3-byte start
/// code and 4 bytes of width and height (RFC 6386 section 9.1).
pub const VP8_KEY_FRAME_CLEAR_LEN: usize = 10;

/// Clear prefix of a VP8 frame that is not a key frame: the 3-byte frame tag.
pub const VP8_DELTA_FRAME_CLEAR_LEN: usize = 3;

/// Why a byte string is not a sealed media frame. Every variant is a refusal:
/// nothing of the input is returned to the caller.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MediaFrameError {
    /// The input is shorter than the layout its trailer declares, or shorter
    /// than any sealed media frame can be.
    #[error("media frame too short: needed {needed} bytes, had {had}")]
    TooShort {
        /// Bytes the declared layout requires.
        needed: usize,
        /// Bytes present.
        had: usize,
    },

    /// The last byte is not [`MEDIA_FRAME_VERSION`]. A frame in the ADR-0013
    /// header-first layout ends in a tag byte and usually stops here.
    #[error("media frame version {0:#04x} is not supported")]
    UnknownVersion(u8),

    /// The trailer claims a clear prefix longer than [`MAX_CLEAR_PREFIX_LEN`],
    /// or a sender asked for one.
    #[error("clear prefix of {len} bytes exceeds the maximum {max}")]
    ClearPrefixTooLong {
        /// The length claimed or asked for.
        len: usize,
        /// [`MAX_CLEAR_PREFIX_LEN`].
        max: usize,
    },

    /// The trailer claims an SFrame header length outside `1..=17`.
    #[error("sframe header length {0} is outside 1..=17")]
    BadHeaderLength(usize),

    /// The bytes at the header position do not decode as an SFrame header.
    #[error("sframe header: {0}")]
    Header(#[from] HeaderError),

    /// The SFrame header decodes, but to a length other than the one the
    /// trailer states.
    #[error("sframe header is {decoded} bytes, the trailer says {declared}")]
    HeaderLengthMismatch {
        /// Length the trailer states.
        declared: usize,
        /// Length the header's own first byte implies.
        decoded: usize,
    },
}

/// The four regions of a sealed media frame, borrowed from the wire, plus the
/// decoded SFrame header.
///
/// Produced by [`MediaFrameParts::split`], which checks every length against
/// the input before taking a slice. Nothing here is authenticated yet: the
/// regions are what the AEAD open is then run over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaFrameParts<'a> {
    /// The codec header bytes left in the clear (possibly empty).
    pub clear_prefix: &'a [u8],
    /// The AEAD ciphertext with its 16-byte tag.
    pub ciphertext: &'a [u8],
    /// The SFrame header exactly as it is on the wire.
    pub header_bytes: &'a [u8],
    /// The 3-byte trailer exactly as it is on the wire.
    pub trailer: &'a [u8],
    /// The decoded SFrame header (KID and counter).
    pub header: SframeHeader,
}

impl<'a> MediaFrameParts<'a> {
    /// Split a sealed media frame into its regions by reading the trailer.
    ///
    /// Needs no knowledge of the codec and no key. Total: any byte string
    /// yields either the parts or an error, never a panic.
    ///
    /// # Errors
    ///
    /// A [`MediaFrameError`] naming the first check that failed: the minimum
    /// length, the version, the two declared lengths, the total length they
    /// imply, the SFrame header's own decoding, and that the header fills its
    /// declared length exactly.
    pub fn split(wire: &'a [u8]) -> Result<Self, MediaFrameError> {
        let had = wire.len();
        let too_short = |needed: usize| MediaFrameError::TooShort { needed, had };
        if had < MIN_MEDIA_FRAME_LEN {
            return Err(too_short(MIN_MEDIA_FRAME_LEN));
        }
        let trailer_start = had - MEDIA_FRAME_TRAILER_LEN;
        let trailer = wire
            .get(trailer_start..)
            .ok_or_else(|| too_short(MIN_MEDIA_FRAME_LEN))?;
        let (clear_len, header_len, version) = match *trailer {
            [c, h, v] => (usize::from(c), usize::from(h), v),
            _ => return Err(too_short(MIN_MEDIA_FRAME_LEN)),
        };
        if version != MEDIA_FRAME_VERSION {
            return Err(MediaFrameError::UnknownVersion(version));
        }
        if clear_len > MAX_CLEAR_PREFIX_LEN {
            return Err(MediaFrameError::ClearPrefixTooLong {
                len: clear_len,
                max: MAX_CLEAR_PREFIX_LEN,
            });
        }
        if header_len == 0 || header_len > MAX_HEADER_LEN {
            return Err(MediaFrameError::BadHeaderLength(header_len));
        }
        // All four terms are small constants or one byte each: no overflow.
        let needed = clear_len + MEDIA_FRAME_TAG_LEN + header_len + MEDIA_FRAME_TRAILER_LEN;
        if had < needed {
            return Err(too_short(needed));
        }
        let header_start = trailer_start - header_len;
        let header_bytes = wire
            .get(header_start..trailer_start)
            .ok_or_else(|| too_short(needed))?;
        let clear_prefix = wire.get(..clear_len).ok_or_else(|| too_short(needed))?;
        let ciphertext = wire
            .get(clear_len..header_start)
            .ok_or_else(|| too_short(needed))?;
        let (header, decoded) = SframeHeader::decode(header_bytes)?;
        if decoded != header_len {
            return Err(MediaFrameError::HeaderLengthMismatch {
                declared: header_len,
                decoded,
            });
        }
        Ok(Self {
            clear_prefix,
            ciphertext,
            header_bytes,
            trailer,
            header,
        })
    }

    /// The AEAD additional data of this frame:
    /// `sframe_header || clear_prefix || trailer`, from the wire bytes.
    #[must_use]
    pub fn aad(&self) -> Vec<u8> {
        media_frame_aad(self.header_bytes, self.clear_prefix, self.trailer)
    }
}

/// Compose the AEAD additional data of a sealed media frame:
/// `sframe_header || clear_prefix || trailer`.
///
/// The split of the three is unambiguous from the bytes alone: the trailer is
/// the last three bytes and states the other two lengths. It can never equal
/// the additional data of the ADR-0013 layout (an SFrame header alone), which
/// is exactly as long as its own first byte says, where this is at least
/// three bytes longer than the header it starts with.
#[must_use]
pub fn media_frame_aad(header_bytes: &[u8], clear_prefix: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header_bytes.len() + clear_prefix.len() + trailer.len());
    aad.extend_from_slice(header_bytes);
    aad.extend_from_slice(clear_prefix);
    aad.extend_from_slice(trailer);
    aad
}

/// Build the trailer for a frame with `clear_len` clear bytes and an SFrame
/// header of `header_len` bytes.
///
/// # Errors
///
/// [`MediaFrameError::ClearPrefixTooLong`] or
/// [`MediaFrameError::BadHeaderLength`] when a length is outside what a
/// receiver accepts, so a sender can never emit a frame its peers refuse.
pub fn media_frame_trailer(
    clear_len: usize,
    header_len: usize,
) -> Result<[u8; MEDIA_FRAME_TRAILER_LEN], MediaFrameError> {
    if clear_len > MAX_CLEAR_PREFIX_LEN {
        return Err(MediaFrameError::ClearPrefixTooLong {
            len: clear_len,
            max: MAX_CLEAR_PREFIX_LEN,
        });
    }
    if header_len == 0 || header_len > MAX_HEADER_LEN {
        return Err(MediaFrameError::BadHeaderLength(header_len));
    }
    let clear = u8::try_from(clear_len).map_err(|_| MediaFrameError::ClearPrefixTooLong {
        len: clear_len,
        max: MAX_CLEAR_PREFIX_LEN,
    })?;
    let header =
        u8::try_from(header_len).map_err(|_| MediaFrameError::BadHeaderLength(header_len))?;
    Ok([clear, header, MEDIA_FRAME_VERSION])
}

/// The codec of a frame, as far as the clear prefix depends on it.
///
/// The sender states it; the receiver never needs it (the trailer carries the
/// lengths). The numeric values cross the FFI boundaries and are frozen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MediaCodec {
    /// Any codec without a rule here, and a codec the sender could not
    /// determine. Nothing is left in the clear: the frame is as secret as
    /// under the ADR-0013 layout, and a receiver without the generic frame
    /// descriptor will not decode it.
    Other = 0,
    /// VP8 video (RFC 6386).
    Vp8 = 1,
    /// Opus audio. Nothing is left in the clear.
    Opus = 2,
}

impl MediaCodec {
    /// Map a negotiated RTP codec MIME type (`"video/VP8"`, `"audio/opus"`,
    /// compared without regard to case) to a [`MediaCodec`]. Anything else,
    /// including RED and RTX wrappers, is [`MediaCodec::Other`].
    #[must_use]
    pub fn from_mime(mime: &str) -> Self {
        if mime.eq_ignore_ascii_case("video/vp8") {
            Self::Vp8
        } else if mime.eq_ignore_ascii_case("audio/opus") {
            Self::Opus
        } else {
            Self::Other
        }
    }

    /// The codec for a frozen FFI value; an unknown value is
    /// [`MediaCodec::Other`], which leaves nothing in the clear.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Vp8,
            2 => Self::Opus,
            _ => Self::Other,
        }
    }
}

/// Whether a VP8 frame is a key frame according to its own frame tag: the
/// lowest bit of the first byte is 0 for a key frame (RFC 6386 section 9.1).
/// `None` for an empty frame.
#[must_use]
pub fn vp8_is_key_frame(frame: &[u8]) -> Option<bool> {
    frame.first().map(|tag| tag & 0x01 == 0)
}

/// How many leading bytes of `frame` a sender leaves in the clear for `codec`.
///
/// - VP8: [`VP8_KEY_FRAME_CLEAR_LEN`] for a key frame, else
///   [`VP8_DELTA_FRAME_CLEAR_LEN`]. The frame's own tag bit decides, not what
///   the platform reported about the frame: the prefix exists for a receiver
///   that reads these bytes, so the bytes are what it must agree with. A
///   frame the tag calls a key frame gets the 10 bytes its reader will parse;
///   a frame the tag calls a delta frame exposes 3 whatever else was claimed.
/// - Opus and [`MediaCodec::Other`]: 0.
///
/// Never more than `frame.len()`: a frame shorter than its codec's prefix is
/// left clear whole, and its ciphertext is the tag over an empty plaintext.
#[must_use]
pub fn clear_prefix_len(codec: MediaCodec, frame: &[u8]) -> usize {
    let wanted = match codec {
        MediaCodec::Vp8 => match vp8_is_key_frame(frame) {
            Some(true) => VP8_KEY_FRAME_CLEAR_LEN,
            Some(false) => VP8_DELTA_FRAME_CLEAR_LEN,
            None => 0,
        },
        MediaCodec::Opus | MediaCodec::Other => 0,
    };
    wanted.min(frame.len())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::call_key_ring::{CallKeyRing, OVERLAP_DEFAULT_MS};
    use crate::sframe::{CallKey, ParticipantKey, CALL_KEY_LEN};

    const SEED: [u8; CALL_KEY_LEN] = [0x42; CALL_KEY_LEN];
    const OTHER_SEED: [u8; CALL_KEY_LEN] = [0x24; CALL_KEY_LEN];
    const LEAF: u32 = 3;
    const CAMERA: u32 = 1;

    /// A VP8 key frame: frame tag (low bit 0), start code `9d 01 2a`,
    /// 640 x 480, then stand-in compressed data.
    fn vp8_key_frame() -> Vec<u8> {
        let mut frame = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01];
        frame.extend_from_slice(b"lakko media frame golden v1");
        frame
    }

    /// A VP8 delta frame: frame tag with the low bit 1.
    fn vp8_delta_frame() -> Vec<u8> {
        let mut frame = vec![0x11, 0x02, 0x00];
        frame.extend_from_slice(b"inter-frame residual");
        frame
    }

    fn sender(leaf: u32, stream: u32) -> ParticipantKey {
        CallKey::from_seed(SEED)
            .participant_key_for(leaf, stream)
            .expect("participant key")
    }

    fn receiver(leaf: u32, stream: u32) -> ParticipantKey {
        sender(leaf, stream)
    }

    fn seal(codec: MediaCodec, frame: &[u8]) -> Vec<u8> {
        sender(LEAF, CAMERA)
            .seal_media_frame_for_codec(frame, codec)
            .expect("seal")
    }

    // ---- the prefix rule ---------------------------------------------------

    #[test]
    fn prefix_rule_per_codec() {
        assert_eq!(clear_prefix_len(MediaCodec::Vp8, &vp8_key_frame()), 10);
        assert_eq!(clear_prefix_len(MediaCodec::Vp8, &vp8_delta_frame()), 3);
        assert_eq!(clear_prefix_len(MediaCodec::Opus, &[0x78, 1, 2, 3]), 0);
        assert_eq!(clear_prefix_len(MediaCodec::Other, &vp8_key_frame()), 0);
        // Shorter than the prefix: the whole frame, never more.
        assert_eq!(clear_prefix_len(MediaCodec::Vp8, &[0x10, 0x02]), 2);
        assert_eq!(clear_prefix_len(MediaCodec::Vp8, &[0x11]), 1);
        assert_eq!(clear_prefix_len(MediaCodec::Vp8, &[]), 0);
    }

    #[test]
    fn vp8_key_frame_is_the_low_bit_of_the_first_byte() {
        assert_eq!(vp8_is_key_frame(&[0x10]), Some(true));
        assert_eq!(vp8_is_key_frame(&[0x11]), Some(false));
        assert_eq!(vp8_is_key_frame(&[]), None);
    }

    #[test]
    fn codec_from_mime_and_from_u8() {
        assert_eq!(MediaCodec::from_mime("video/VP8"), MediaCodec::Vp8);
        assert_eq!(MediaCodec::from_mime("VIDEO/vp8"), MediaCodec::Vp8);
        assert_eq!(MediaCodec::from_mime("audio/opus"), MediaCodec::Opus);
        assert_eq!(MediaCodec::from_mime("video/H264"), MediaCodec::Other);
        assert_eq!(MediaCodec::from_mime("video/VP9"), MediaCodec::Other);
        assert_eq!(MediaCodec::from_mime("audio/red"), MediaCodec::Other);
        assert_eq!(MediaCodec::from_mime(""), MediaCodec::Other);
        for codec in [MediaCodec::Other, MediaCodec::Vp8, MediaCodec::Opus] {
            assert_eq!(MediaCodec::from_u8(codec as u8), codec);
        }
        assert_eq!(MediaCodec::from_u8(200), MediaCodec::Other);
    }

    // ---- round trips -------------------------------------------------------

    fn assert_round_trip(codec: MediaCodec, frame: &[u8], expected_clear: usize) {
        let wire = seal(codec, frame);
        let parts = MediaFrameParts::split(&wire).expect("split");
        assert_eq!(parts.clear_prefix.len(), expected_clear);
        // The clear prefix is the first bytes of the input, at the start of
        // the sealed frame.
        assert_eq!(&wire[..expected_clear], &frame[..expected_clear]);
        assert_eq!(parts.clear_prefix, &frame[..expected_clear]);
        assert_eq!(
            parts.ciphertext.len(),
            frame.len() - expected_clear + MEDIA_FRAME_TAG_LEN
        );
        assert_eq!(
            parts.header.kid,
            SframeHeader::kid_for(LEAF, CAMERA).unwrap()
        );
        assert!(wire.len() <= frame.len() + MAX_MEDIA_FRAME_OVERHEAD);
        let opened = receiver(LEAF, CAMERA)
            .open_media_frame(&wire)
            .expect("open");
        assert_eq!(opened, frame);
    }

    #[test]
    fn vp8_key_frame_round_trips_with_ten_clear_bytes() {
        assert_round_trip(MediaCodec::Vp8, &vp8_key_frame(), 10);
    }

    #[test]
    fn vp8_delta_frame_round_trips_with_three_clear_bytes() {
        assert_round_trip(MediaCodec::Vp8, &vp8_delta_frame(), 3);
    }

    #[test]
    fn audio_and_unknown_codec_round_trip_with_nothing_clear() {
        assert_round_trip(MediaCodec::Opus, b"\x78opus payload bytes", 0);
        assert_round_trip(MediaCodec::Other, &vp8_key_frame(), 0);
    }

    #[test]
    fn frames_shorter_than_the_prefix_round_trip() {
        // A key-frame tag with fewer than 10 bytes, a delta tag with fewer
        // than 3: sealed with the prefix they have.
        assert_round_trip(MediaCodec::Vp8, &[0x10, 0x02, 0x00, 0x9d], 4);
        assert_round_trip(MediaCodec::Vp8, &[0x11, 0x02], 2);
        assert_round_trip(MediaCodec::Vp8, &[0x11], 1);
    }

    #[test]
    fn empty_frame_round_trips() {
        for codec in [MediaCodec::Vp8, MediaCodec::Opus, MediaCodec::Other] {
            assert_round_trip(codec, &[], 0);
        }
    }

    #[test]
    fn a_sealed_vp8_frame_starts_with_the_vp8_header() {
        // What a receiver without the frame descriptor reads: the low bit of
        // byte 0 answers "key frame?" correctly for both kinds.
        let key = seal(MediaCodec::Vp8, &vp8_key_frame());
        let delta = seal(MediaCodec::Vp8, &vp8_delta_frame());
        assert_eq!(vp8_is_key_frame(&key), Some(true));
        assert_eq!(vp8_is_key_frame(&delta), Some(false));
        assert_eq!(&key[..10], &vp8_key_frame()[..10]);
        assert_eq!(&delta[..3], &vp8_delta_frame()[..3]);
    }

    #[test]
    fn the_body_after_the_prefix_is_not_on_the_wire() {
        let frame = vp8_key_frame();
        let wire = seal(MediaCodec::Vp8, &frame);
        let body = &frame[10..];
        assert!(!wire.windows(body.len()).any(|w| w == body));
    }

    #[test]
    fn explicit_prefix_is_clamped_to_the_frame_and_bounded() {
        let mut key = sender(LEAF, CAMERA);
        let wire = key.seal_media_frame(&[1, 2, 3], 10).expect("clamped");
        assert_eq!(
            MediaFrameParts::split(&wire).unwrap().clear_prefix,
            &[1, 2, 3]
        );
        let before = key.counter();
        assert!(key
            .seal_media_frame(&[0u8; 64], MAX_CLEAR_PREFIX_LEN + 1)
            .is_err());
        assert_eq!(key.counter(), before, "a refused seal consumes no counter");
        assert!(key
            .seal_media_frame(&[0u8; 64], MAX_CLEAR_PREFIX_LEN)
            .is_ok());
    }

    #[test]
    fn counters_advance_once_per_sealed_frame() {
        let mut key = sender(LEAF, CAMERA);
        let start = key.counter();
        let a = key
            .seal_media_frame_for_codec(&vp8_key_frame(), MediaCodec::Vp8)
            .unwrap();
        let b = key
            .seal_media_frame_for_codec(&vp8_delta_frame(), MediaCodec::Vp8)
            .unwrap();
        assert_eq!(MediaFrameParts::split(&a).unwrap().header.ctr, start + 1);
        assert_eq!(MediaFrameParts::split(&b).unwrap().header.ctr, start + 2);
    }

    #[test]
    fn counter_exhaustion_seals_nothing() {
        let mut key = sender(LEAF, CAMERA);
        key.set_counter_for_test(u64::MAX);
        assert!(matches!(
            key.seal_media_frame(&vp8_key_frame(), 10),
            Err(crate::Error::CounterExhausted)
        ));
        assert_eq!(key.counter(), u64::MAX);
    }

    // ---- tampering ---------------------------------------------------------

    #[test]
    fn any_single_bit_flip_is_refused() {
        for frame in [vp8_key_frame(), vp8_delta_frame(), b"audio".to_vec()] {
            let codec = MediaCodec::Vp8;
            let wire = seal(codec, &frame);
            let key = receiver(LEAF, CAMERA);
            assert!(key.open_media_frame(&wire).is_ok());
            for index in 0..wire.len() {
                for bit in 0..8 {
                    let mut bad = wire.clone();
                    bad[index] ^= 1 << bit;
                    assert!(
                        key.open_media_frame(&bad).is_err(),
                        "flipping bit {bit} of byte {index} was accepted"
                    );
                }
            }
        }
    }

    #[test]
    fn each_region_is_authenticated() {
        let frame = vp8_key_frame();
        let wire = seal(MediaCodec::Vp8, &frame);
        let key = receiver(LEAF, CAMERA);
        let parts = MediaFrameParts::split(&wire).unwrap();
        assert!(!parts.header_bytes.is_empty());
        let header_last = wire.len() - MEDIA_FRAME_TRAILER_LEN - 1;

        // A clear byte: turn the key frame into a delta frame for a reader.
        let mut bad = wire.clone();
        bad[0] |= 0x01;
        assert!(key.open_media_frame(&bad).is_err());
        // A clear byte: change the advertised width.
        let mut bad = wire.clone();
        bad[6] ^= 0x40;
        assert!(key.open_media_frame(&bad).is_err());
        // The ciphertext.
        let mut bad = wire.clone();
        bad[12] ^= 0x01;
        assert!(key.open_media_frame(&bad).is_err());
        // The SFrame header (the last byte of its counter).
        let mut bad = wire.clone();
        bad[header_last] ^= 0x01;
        assert!(key.open_media_frame(&bad).is_err());
        // The trailer: move the boundary between clear and ciphertext. The
        // lengths still add up, so only the AEAD catches it.
        let mut bad = wire.clone();
        let at = bad.len() - MEDIA_FRAME_TRAILER_LEN;
        bad[at] -= 1;
        assert!(MediaFrameParts::split(&bad).is_ok());
        assert!(key.open_media_frame(&bad).is_err());
    }

    #[test]
    fn a_frame_under_another_seed_leaf_or_stream_is_refused() {
        let wire = seal(MediaCodec::Vp8, &vp8_key_frame());
        let other_seed = CallKey::from_seed(OTHER_SEED)
            .participant_key_for(LEAF, CAMERA)
            .unwrap();
        assert!(other_seed.open_media_frame(&wire).is_err());
        assert!(receiver(LEAF + 1, CAMERA).open_media_frame(&wire).is_err());
        assert!(receiver(LEAF, CAMERA + 1).open_media_frame(&wire).is_err());
    }

    #[test]
    fn a_trailer_moved_onto_another_frames_body_is_refused() {
        let mut key = sender(LEAF, CAMERA);
        let a = key.seal_media_frame(b"frame number one", 0).unwrap();
        let b = key.seal_media_frame(b"frame number two", 0).unwrap();
        // Splice b's header + trailer onto a's ciphertext.
        let parts_b = MediaFrameParts::split(&b).unwrap();
        let parts_a = MediaFrameParts::split(&a).unwrap();
        let mut spliced = parts_a.ciphertext.to_vec();
        spliced.extend_from_slice(parts_b.header_bytes);
        spliced.extend_from_slice(parts_b.trailer);
        assert!(MediaFrameParts::split(&spliced).is_ok());
        assert!(receiver(LEAF, CAMERA).open_media_frame(&spliced).is_err());
    }

    // ---- the two layouts do not open as each other -------------------------

    #[test]
    fn the_header_first_layout_is_refused_by_the_new_open() {
        let frame = vp8_key_frame();
        let key = receiver(LEAF, CAMERA);
        // Many counters, so every value of the old frame's last bytes (tag
        // bytes) is exercised, including ones that pass the trailer checks.
        let mut old_sender = sender(LEAF, CAMERA);
        for _ in 0..2_000 {
            let old = old_sender.seal_framed(&frame).expect("old seal");
            assert!(key.open_media_frame(&old).is_err());
            assert!(key.open_framed(&old).is_ok(), "the old open still reads it");
        }
        let ring = CallKeyRing::from_seed(SEED);
        let old = old_sender.seal_framed(&frame).unwrap();
        assert!(ring.try_open_media_frame_with_overlap(&old).is_err());
    }

    #[test]
    fn the_new_layout_is_refused_by_the_header_first_open() {
        let key = receiver(LEAF, CAMERA);
        let mut new_sender = sender(LEAF, CAMERA);
        for codec in [MediaCodec::Vp8, MediaCodec::Opus] {
            for _ in 0..500 {
                let wire = new_sender
                    .seal_media_frame_for_codec(&vp8_delta_frame(), codec)
                    .unwrap();
                assert!(key.open_framed(&wire).is_err());
            }
        }
    }

    #[test]
    fn the_two_layouts_never_share_additional_data() {
        // The header-first AAD is a header alone; the new AAD starts with a
        // header and is at least the trailer longer.
        let header = SframeHeader {
            kid: 0x0301,
            ctr: 7,
        }
        .encode();
        let trailer = media_frame_trailer(0, header.len()).unwrap();
        let aad = media_frame_aad(&header, &[], &trailer);
        assert_eq!(aad.len(), header.len() + MEDIA_FRAME_TRAILER_LEN);
        assert_ne!(aad, header);
    }

    // ---- keys --------------------------------------------------------------

    #[test]
    fn two_streams_of_one_leaf_seal_under_distinct_keys() {
        let frame = vp8_key_frame();
        let mut camera = sender(LEAF, 1);
        let mut light = sender(LEAF, 3);
        assert_ne!(camera.key_bytes(), light.key_bytes());
        assert_ne!(camera.nonce_salt(), light.nonce_salt());
        camera.set_counter_for_test(0);
        light.set_counter_for_test(0);
        let a = camera.seal_media_frame(&frame, 10).unwrap();
        let b = light.seal_media_frame(&frame, 10).unwrap();
        let (pa, pb) = (
            MediaFrameParts::split(&a).unwrap(),
            MediaFrameParts::split(&b).unwrap(),
        );
        assert_eq!(pa.header.ctr, pb.header.ctr);
        assert_ne!(pa.header.kid, pb.header.kid);
        assert_ne!(pa.ciphertext, pb.ciphertext);
        // Each opens only under its own stream's key.
        assert!(receiver(LEAF, 1).open_media_frame(&a).is_ok());
        assert!(receiver(LEAF, 3).open_media_frame(&a).is_err());
        assert!(receiver(LEAF, 3).open_media_frame(&b).is_ok());
        assert!(receiver(LEAF, 1).open_media_frame(&b).is_err());
    }

    // ---- the key ring (ADR-0029) -------------------------------------------

    fn ring_seal(ring: &CallKeyRing, frame: &[u8]) -> Vec<u8> {
        let kid = SframeHeader::kid_for(LEAF, CAMERA).unwrap();
        ring.participant_key(kid)
            .unwrap()
            .seal_media_frame_for_codec(frame, MediaCodec::Vp8)
            .unwrap()
    }

    #[test]
    fn the_ring_opens_old_and_new_epoch_frames_inside_the_window() {
        let mut ring = CallKeyRing::from_seed(SEED);
        let old = ring_seal(&ring, &vp8_key_frame());
        ring.rekey_from_seed_at(OTHER_SEED, 1_000, OVERLAP_DEFAULT_MS);
        let new = ring_seal(&ring, &vp8_delta_frame());
        assert_eq!(
            ring.try_open_media_frame_with_overlap(&old).unwrap(),
            vp8_key_frame()
        );
        assert_eq!(
            ring.try_open_media_frame_with_overlap(&new).unwrap(),
            vp8_delta_frame()
        );
        // After the window the previous key is gone.
        ring.prune_expired(1_000 + OVERLAP_DEFAULT_MS);
        assert!(ring.try_open_media_frame_with_overlap(&old).is_err());
        assert!(ring.try_open_media_frame_with_overlap(&new).is_ok());
    }

    #[test]
    fn the_ring_refuses_garbage_and_a_foreign_seed() {
        let ring = CallKeyRing::from_seed(SEED);
        assert!(ring.try_open_media_frame_with_overlap(&[]).is_err());
        assert!(ring.try_open_media_frame_with_overlap(&[0u8; 64]).is_err());
        let foreign = CallKeyRing::from_seed(OTHER_SEED);
        let wire = ring_seal(&foreign, &vp8_key_frame());
        assert!(ring.try_open_media_frame_with_overlap(&wire).is_err());
    }

    // ---- the trailer parser ------------------------------------------------

    #[test]
    fn split_refuses_each_malformed_trailer() {
        let wire = seal(MediaCodec::Vp8, &vp8_key_frame());
        let n = wire.len();
        assert!(matches!(
            MediaFrameParts::split(&wire[..MIN_MEDIA_FRAME_LEN - 1]),
            Err(MediaFrameError::TooShort { .. })
        ));
        let mut bad = wire.clone();
        bad[n - 1] = 0x02;
        assert_eq!(
            MediaFrameParts::split(&bad),
            Err(MediaFrameError::UnknownVersion(0x02))
        );
        let mut bad = wire.clone();
        bad[n - 3] = 17;
        assert!(matches!(
            MediaFrameParts::split(&bad),
            Err(MediaFrameError::ClearPrefixTooLong { len: 17, .. })
        ));
        for header_len in [0u8, 18, 255] {
            let mut bad = wire.clone();
            bad[n - 2] = header_len;
            assert_eq!(
                MediaFrameParts::split(&bad),
                Err(MediaFrameError::BadHeaderLength(usize::from(header_len)))
            );
        }
        // A header length that is in range but not the header's own.
        let mut bad = wire.clone();
        bad[n - 2] -= 1;
        assert!(MediaFrameParts::split(&bad).is_err());
        // Declared lengths that exceed the frame.
        let mut short = vec![0u8; MIN_MEDIA_FRAME_LEN];
        short[MIN_MEDIA_FRAME_LEN - 1] = MEDIA_FRAME_VERSION;
        short[MIN_MEDIA_FRAME_LEN - 2] = 1;
        short[MIN_MEDIA_FRAME_LEN - 3] = 16;
        assert!(matches!(
            MediaFrameParts::split(&short),
            Err(MediaFrameError::TooShort {
                needed: 36,
                had: 20
            })
        ));
    }

    #[test]
    fn trailer_builder_refuses_what_split_refuses() {
        assert!(media_frame_trailer(MAX_CLEAR_PREFIX_LEN, MAX_HEADER_LEN).is_ok());
        assert!(media_frame_trailer(MAX_CLEAR_PREFIX_LEN + 1, 1).is_err());
        assert!(media_frame_trailer(0, 0).is_err());
        assert!(media_frame_trailer(0, MAX_HEADER_LEN + 1).is_err());
    }

    #[test]
    fn every_truncation_of_a_sealed_frame_is_refused() {
        let wire = seal(MediaCodec::Vp8, &vp8_key_frame());
        let key = receiver(LEAF, CAMERA);
        for len in 0..wire.len() {
            assert!(key.open_media_frame(&wire[..len]).is_err(), "prefix {len}");
            assert!(key.open_media_frame(&wire[wire.len() - len..]).is_err());
        }
    }

    // ---- golden vector -----------------------------------------------------

    /// Seed `0x42` x 32, leaf 3, stream 1 (KID `0x0301`), counter 1, the VP8
    /// key frame of [`vp8_key_frame`]. Pinned for every stack that adopts
    /// the ADR-0071 layout.
    const GOLDEN_MEDIA_FRAME_HEX: &str =
        "1002009d012a8002e0012dc56b47f2bfd3ebf051d8893d6d2b2b355f29f3980b60\
         e19c2555c9ad3a6ca9e95f08b228768c0f59a6509103010a0301";

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn golden_vector_seals_to_the_pinned_bytes() {
        let mut key = sender(LEAF, CAMERA);
        key.set_counter_for_test(0);
        let wire = key
            .seal_media_frame_for_codec(&vp8_key_frame(), MediaCodec::Vp8)
            .unwrap();
        assert_eq!(hex(&wire), GOLDEN_MEDIA_FRAME_HEX);
    }

    #[test]
    fn golden_vector_opens_to_the_frame() {
        let wire = unhex(GOLDEN_MEDIA_FRAME_HEX);
        let parts = MediaFrameParts::split(&wire).unwrap();
        assert_eq!(
            parts.header,
            SframeHeader {
                kid: 0x0301,
                ctr: 1
            }
        );
        assert_eq!(parts.header_bytes, &[0x91, 0x03, 0x01]);
        assert_eq!(parts.trailer, &[0x0a, 0x03, 0x01]);
        assert_eq!(parts.clear_prefix, &vp8_key_frame()[..10]);
        assert_eq!(
            receiver(LEAF, CAMERA).open_media_frame(&wire).unwrap(),
            vp8_key_frame()
        );
        assert_eq!(
            CallKeyRing::from_seed(SEED)
                .try_open_media_frame_with_overlap(&wire)
                .unwrap(),
            vp8_key_frame()
        );
    }

    // ---- arbitrary input ---------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// `split` and both opens are total on arbitrary bytes, and a split
        /// that succeeds accounts for every byte of the input.
        #[test]
        fn open_never_panics_and_never_accepts_arbitrary_bytes(
            bytes in proptest::collection::vec(any::<u8>(), 0..256)
        ) {
            if let Ok(parts) = MediaFrameParts::split(&bytes) {
                prop_assert_eq!(
                    parts.clear_prefix.len()
                        + parts.ciphertext.len()
                        + parts.header_bytes.len()
                        + parts.trailer.len(),
                    bytes.len()
                );
                prop_assert!(parts.clear_prefix.len() <= MAX_CLEAR_PREFIX_LEN);
                prop_assert!(parts.ciphertext.len() >= MEDIA_FRAME_TAG_LEN);
                prop_assert!(parts.header_bytes.len() <= MAX_HEADER_LEN);
            }
            let ring = CallKeyRing::from_seed(SEED);
            prop_assert!(ring.try_open_media_frame_with_overlap(&bytes).is_err());
            prop_assert!(receiver(LEAF, CAMERA).open_media_frame(&bytes).is_err());
        }

        /// The same with a well-formed trailer forced on, so the checks past
        /// the version byte are reached on most cases.
        #[test]
        fn open_never_panics_behind_a_plausible_trailer(
            mut bytes in proptest::collection::vec(any::<u8>(), 3..128),
            clear in 0u8..=20,
            header in 0u8..=20,
        ) {
            let n = bytes.len();
            bytes[n - 1] = MEDIA_FRAME_VERSION;
            bytes[n - 2] = header;
            bytes[n - 3] = clear;
            let _ = MediaFrameParts::split(&bytes);
            let ring = CallKeyRing::from_seed(SEED);
            prop_assert!(ring.try_open_media_frame_with_overlap(&bytes).is_err());
        }

        /// Any frame, any admissible prefix: the wire starts with the clear
        /// prefix and opens to the frame.
        #[test]
        fn seal_then_open_is_the_identity(
            frame in proptest::collection::vec(any::<u8>(), 0..300),
            clear in 0usize..=MAX_CLEAR_PREFIX_LEN,
            leaf in any::<u32>(),
            stream in 0u32..=255,
        ) {
            let call_key = CallKey::from_seed(SEED);
            let mut key = call_key.participant_key_for(leaf, stream).unwrap();
            let wire = key.seal_media_frame(&frame, clear).unwrap();
            let kept = clear.min(frame.len());
            prop_assert_eq!(&wire[..kept], &frame[..kept]);
            let ring = CallKeyRing::from_seed(SEED);
            prop_assert_eq!(ring.try_open_media_frame_with_overlap(&wire).unwrap(), frame);
        }
    }
}
