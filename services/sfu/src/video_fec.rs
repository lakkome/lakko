// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Byte-level handling of video RED and ULPFEC
//! ([ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4).
//!
//! ## What arrives
//!
//! A libwebrtc sender that negotiated both `red/90000` and `ulpfec/90000`
//! sends every video packet under the `red` payload type. The RTP payload is
//! then one RED header byte followed by a body:
//!
//! ```text
//!  0 1 2 3 4 5 6 7
//! +-+-+-+-+-+-+-+-+
//! |F|  block PT   |   F = 0: this is the only block (RFC 2198 section 3)
//! +-+-+-+-+-+-+-+-+
//! ```
//!
//! - block PT = the media payload type: the body is the media payload exactly
//!   as it would have been sent without RED (for Lakko, SFrame ciphertext);
//! - block PT = the `ulpfec` payload type: the body is an FEC packet
//!   (RFC 5109), on the same SSRC and in the same sequence-number space as
//!   the media.
//!
//! ## What the SFU does with it
//!
//! - **RED media to a subscriber that negotiated the pair**: the header byte
//!   is rewritten to the subscriber's media payload type, the body is kept
//!   ([`red_rewrap`]).
//! - **RED media to a subscriber that did not**: the header byte is removed
//!   and the body is sent under the subscriber's media payload type
//!   ([`red_body`]).
//! - **FEC to a subscriber that negotiated the pair**: the header byte is
//!   rewritten to the subscriber's `ulpfec` payload type and the FEC header's
//!   `SN base` is moved by the downstream stream's sequence offset
//!   ([`forward_fec`]). The packet is refused ([`FecDrop`]) when the recovery
//!   fields cannot be kept true.
//! - **FEC to anyone else**: not sent.
//!
//! ## Opacity (invariant #2, C-39)
//!
//! Everything read here is cleartext transport framing that sits outside the
//! SFrame-sealed frame: the one RED header byte, and the first 14 or 18 bytes
//! of an FEC packet (the RFC 5109 FEC header and level-0 header). A client
//! computes FEC over packets that already carry SFrame ciphertext, so the FEC
//! body is an XOR of ciphertext and tells the SFU nothing. No function in
//! this module reads, compares or alters a byte of a media body or of an FEC
//! body; they are copied.
//!
//! ## What this module does NOT make true
//!
//! An FEC packet repairs a lost media packet only if the receiver holds the
//! other protected packets byte for byte as the sender's FEC encoder saw
//! them. RFC 5109 section 7.3 protects everything after the 12-byte fixed
//! RTP header: the CSRC list, the **header extension block**, the payload
//! and padding. libwebrtc does the same (`forward_error_correction.cc`,
//! `XorPayloads` reads from `kRtpHeaderSize`), and its receiver zeroes only
//! the extensions it calls mutable (abs-send-time, transport-wide sequence
//! number, transmission offset, part of video-timing) before decoding
//! (`ulpfec_receiver.cc`, `RtpPacket::ZeroMutableExtensions`); `mid`, the
//! frame descriptor and video orientation are left as received.
//!
//! The SFU does not forward a publisher's header extension block. Each leg's
//! block is written by `str0m` from parsed values, with that leg's own `mid`,
//! its own extension ids and its own choice of when `mid` is present. So a
//! media packet a subscriber receives is in general NOT byte-identical, after
//! the fixed header, to the packet the publisher's FEC was computed over, and
//! a recovery that uses it produces a packet with a wrong extension block,
//! wrong length or wrong payload. Keeping `SN base`, the timestamps and the
//! payload type right, which is all this module does, is necessary and not
//! sufficient. See `services/sfu/README.md`, "Video FEC".
//!
//! Pure functions: no I/O, no state, no panic on any input.

/// Length of the RED header for a single-block packet.
pub const RED_HEADER_LEN: usize = 1;

/// `F` bit of a RED block header: set when another block header follows.
const RED_FOLLOW_BIT: u8 = 0x80;

/// `L` bit of the FEC header: the mask is 48 bits rather than 16.
const FEC_LONG_MASK_BIT: u8 = 0x40;

/// Offset of `SN base` within an FEC packet (RFC 5109 section 7.3).
const FEC_SN_BASE_OFFSET: usize = 2;

/// Length of an FEC packet's headers with a 16-bit mask: the 10-byte FEC
/// header, a 2-byte protection length and the 2-byte mask.
pub const FEC_HEADER_LEN_SHORT_MASK: usize = 14;

/// Length of an FEC packet's headers with a 48-bit mask.
pub const FEC_HEADER_LEN_LONG_MASK: usize = 18;

/// Why a RED payload could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedError {
    /// The payload has no bytes, so no header.
    Empty,
    /// The header announces a further block. Video RED carries exactly one;
    /// libwebrtc drops a video RED packet with more.
    MultipleBlocks,
}

/// The payload type named by the header byte of a single-block RED payload.
///
/// Reads `payload[0]` and nothing else.
///
/// # Errors
///
/// [`RedError::Empty`] for an empty payload, [`RedError::MultipleBlocks`]
/// when the `F` bit is set.
pub fn red_block_pt(payload: &[u8]) -> Result<u8, RedError> {
    let header = *payload.first().ok_or(RedError::Empty)?;
    if header & RED_FOLLOW_BIT != 0 {
        return Err(RedError::MultipleBlocks);
    }
    Ok(header)
}

/// The body of a single-block RED payload: everything after the header
/// byte. `None` for an empty payload. The body is returned, not read.
#[must_use]
pub fn red_body(payload: &[u8]) -> Option<&[u8]> {
    payload.get(RED_HEADER_LEN..)
}

/// A copy of a single-block RED payload whose header names `block_pt`
/// instead. The body is copied unchanged. `None` for an empty payload or a
/// `block_pt` that is not a payload type (above 127).
#[must_use]
pub fn red_rewrap(payload: &[u8], block_pt: u8) -> Option<Vec<u8>> {
    let body = red_body(payload)?;
    if block_pt & RED_FOLLOW_BIT != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(payload.len());
    out.push(block_pt);
    out.extend_from_slice(body);
    Some(out)
}

/// The cleartext header fields of an FEC packet (RFC 5109 sections 7.3 and
/// 7.4) that say which packets it protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecHeader {
    /// `SN base`: the lowest sequence number the mask can refer to.
    pub sn_base: u16,
    /// Whether the mask is 48 bits (`L` set) or 16.
    pub long_mask: bool,
}

impl FecHeader {
    /// Length of the FEC header plus the level-0 header.
    #[must_use]
    pub const fn header_len(&self) -> usize {
        if self.long_mask {
            FEC_HEADER_LEN_LONG_MASK
        } else {
            FEC_HEADER_LEN_SHORT_MASK
        }
    }
}

/// Read the header of an FEC packet (the bytes after the RED header byte).
///
/// Reads the `L` bit and `SN base`; `None` when the packet is shorter than
/// the headers its own `L` bit announces.
#[must_use]
pub fn parse_fec_header(fec: &[u8]) -> Option<FecHeader> {
    let first = *fec.first()?;
    let long_mask = first & FEC_LONG_MASK_BIT != 0;
    let header = FecHeader {
        sn_base: u16::from_be_bytes([
            *fec.get(FEC_SN_BASE_OFFSET)?,
            *fec.get(FEC_SN_BASE_OFFSET + 1)?,
        ]),
        long_mask,
    };
    (fec.len() >= header.header_len()).then_some(header)
}

/// How one downstream stream relates to the publisher's stream, as far as an
/// FEC packet's recovery fields are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecMapping {
    /// Downstream sequence number minus the publisher's, modulo 2^16.
    pub seq_offset: u16,
    /// Downstream RTP timestamp minus the publisher's, modulo 2^32.
    pub time_offset: u32,
    /// The media payload type of the publisher's RED media packets last
    /// forwarded on this stream, and the one they were sent under. `None`
    /// until a RED media packet has been forwarded.
    pub media_pt: Option<(u8, u8)>,
}

/// Why an FEC packet is not forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FecDrop {
    /// The packet is shorter than its own headers.
    Truncated,
    /// The downstream timestamps differ from the publisher's. `TS recovery`
    /// is the XOR of the protected packets' timestamps, and an XOR cannot be
    /// corrected for an added offset without knowing each timestamp.
    TimestampOffset,
    /// No RED media packet has been forwarded on the stream yet, so the
    /// relation between the two legs' media payload types is unknown.
    MediaPayloadTypeUnknown,
    /// The subscriber knows the media under another payload type than the
    /// publisher. `PT recovery` is the XOR of the protected packets' payload
    /// types as the publisher numbered them; a recovered packet would carry
    /// the wrong one. Not corrected here.
    MediaPayloadTypeDiffers,
    /// `block_pt` is not a payload type (above 127).
    BadPayloadType,
}

/// A copy of a RED-wrapped FEC packet for one subscriber: the RED header
/// names `ulpfec_pt`, and `SN base` is moved by the stream's sequence
/// offset (modulo 2^16) so it refers to the sequence numbers the subscriber
/// sees. Every other byte, the mask and the FEC body included, is copied.
///
/// `payload` is the whole RTP payload, RED header byte first.
///
/// # Errors
///
/// A [`FecDrop`] naming the reason the packet cannot be kept valid.
pub fn forward_fec(
    payload: &[u8],
    ulpfec_pt: u8,
    mapping: &FecMapping,
) -> Result<Vec<u8>, FecDrop> {
    let fec = red_body(payload).ok_or(FecDrop::Truncated)?;
    let header = parse_fec_header(fec).ok_or(FecDrop::Truncated)?;
    if ulpfec_pt & RED_FOLLOW_BIT != 0 {
        return Err(FecDrop::BadPayloadType);
    }
    if mapping.time_offset != 0 {
        return Err(FecDrop::TimestampOffset);
    }
    match mapping.media_pt {
        None => return Err(FecDrop::MediaPayloadTypeUnknown),
        Some((source, downstream)) if source != downstream => {
            return Err(FecDrop::MediaPayloadTypeDiffers);
        },
        Some(_) => {},
    }
    let mut out = Vec::with_capacity(payload.len());
    out.push(ulpfec_pt);
    out.extend_from_slice(fec);
    let sn_base = header
        .sn_base
        .wrapping_add(mapping.seq_offset)
        .to_be_bytes();
    // `parse_fec_header` checked the length; the slice is in bounds.
    let at = RED_HEADER_LEN + FEC_SN_BASE_OFFSET;
    if let Some(slot) = out.get_mut(at..at + 2) {
        slot.copy_from_slice(&sn_base);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIA_PT: u8 = 96;
    const FEC_PT: u8 = 118;

    /// An FEC packet with the given `SN base`, mask width and body.
    fn fec_packet(sn_base: u16, long_mask: bool, body: &[u8]) -> Vec<u8> {
        let mut p = vec![
            if long_mask { FEC_LONG_MASK_BIT } else { 0 },
            0x60, // M and PT recovery
        ];
        p.extend_from_slice(&sn_base.to_be_bytes());
        p.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // TS recovery
        p.extend_from_slice(&[0x01, 0x23]); // length recovery
        p.extend_from_slice(&[0x00, 0x40]); // protection length
        p.extend_from_slice(if long_mask {
            &[0xc0, 0, 0, 0, 0, 0]
        } else {
            &[0xc0, 0]
        });
        p.extend_from_slice(body);
        p
    }

    fn red(block_pt: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![block_pt];
        p.extend_from_slice(body);
        p
    }

    const SAME_PT: FecMapping = FecMapping {
        seq_offset: 0,
        time_offset: 0,
        media_pt: Some((MEDIA_PT, MEDIA_PT)),
    };

    #[test]
    fn red_header_names_the_block_payload_type() {
        assert_eq!(red_block_pt(&red(MEDIA_PT, b"body")), Ok(MEDIA_PT));
        assert_eq!(red_block_pt(&red(FEC_PT, b"")), Ok(FEC_PT));
        assert_eq!(red_block_pt(&red(0, b"x")), Ok(0));
        assert_eq!(red_block_pt(&red(127, b"x")), Ok(127));
    }

    #[test]
    fn red_header_with_a_further_block_is_refused() {
        assert_eq!(
            red_block_pt(&[0x80 | MEDIA_PT, 0, 0, 0, MEDIA_PT, 1]),
            Err(RedError::MultipleBlocks)
        );
        assert_eq!(red_block_pt(&[0xff]), Err(RedError::MultipleBlocks));
    }

    #[test]
    fn empty_and_one_byte_payloads_never_panic() {
        assert_eq!(red_block_pt(&[]), Err(RedError::Empty));
        assert_eq!(red_body(&[]), None);
        assert_eq!(red_rewrap(&[], MEDIA_PT), None);
        assert_eq!(forward_fec(&[], FEC_PT, &SAME_PT), Err(FecDrop::Truncated));

        // One byte: a header and an empty body.
        assert_eq!(red_block_pt(&[MEDIA_PT]), Ok(MEDIA_PT));
        assert_eq!(red_body(&[MEDIA_PT]), Some(&[][..]));
        assert_eq!(red_rewrap(&[MEDIA_PT], 100), Some(vec![100]));
        assert_eq!(
            forward_fec(&[FEC_PT], FEC_PT, &SAME_PT),
            Err(FecDrop::Truncated)
        );
    }

    #[test]
    fn unwrap_returns_the_body_verbatim() {
        let body: Vec<u8> = (0..=255).collect();
        let payload = red(MEDIA_PT, &body);
        assert_eq!(red_body(&payload), Some(&body[..]));
    }

    #[test]
    fn rewrap_changes_only_the_header_byte() {
        let body: Vec<u8> = (0..=255).rev().collect();
        let payload = red(MEDIA_PT, &body);
        let out = red_rewrap(&payload, 100).expect("rewrap");
        assert_eq!(out[0], 100);
        assert_eq!(&out[1..], &body[..]);
        assert_eq!(out.len(), payload.len());
        // Same number: the copy equals the input.
        assert_eq!(red_rewrap(&payload, MEDIA_PT), Some(payload.clone()));
        // Not a payload type.
        assert_eq!(red_rewrap(&payload, 0x80), None);
    }

    #[test]
    fn fec_header_is_read_for_both_mask_widths() {
        let short = fec_packet(4_660, false, b"xor");
        assert_eq!(
            parse_fec_header(&short),
            Some(FecHeader {
                sn_base: 4_660,
                long_mask: false
            })
        );
        let long = fec_packet(65_535, true, b"");
        assert_eq!(
            parse_fec_header(&long),
            Some(FecHeader {
                sn_base: 65_535,
                long_mask: true
            })
        );
    }

    #[test]
    fn a_truncated_fec_header_is_refused_at_every_length() {
        for long_mask in [false, true] {
            let whole = fec_packet(7, long_mask, b"");
            for len in 0..whole.len() {
                assert_eq!(
                    parse_fec_header(&whole[..len]),
                    None,
                    "long_mask={long_mask} len={len}"
                );
                assert_eq!(
                    forward_fec(&red(FEC_PT, &whole[..len]), FEC_PT, &SAME_PT),
                    Err(FecDrop::Truncated),
                    "long_mask={long_mask} len={len}"
                );
            }
            assert!(parse_fec_header(&whole).is_some());
        }
        // A long-mask packet cut to the short-mask length.
        let long = fec_packet(7, true, b"");
        assert_eq!(parse_fec_header(&long[..FEC_HEADER_LEN_SHORT_MASK]), None);
    }

    #[test]
    fn fec_with_no_offsets_changes_only_the_red_header() {
        let fec = fec_packet(1_000, false, b"fec body bytes");
        let out = forward_fec(&red(FEC_PT, &fec), 110, &SAME_PT).expect("forward");
        assert_eq!(out[0], 110);
        assert_eq!(&out[1..], &fec[..]);
    }

    #[test]
    fn fec_sn_base_moves_by_the_sequence_offset() {
        let body = b"fec body bytes";
        for long_mask in [false, true] {
            let fec = fec_packet(1_000, long_mask, body);
            let mapping = FecMapping {
                seq_offset: 234,
                ..SAME_PT
            };
            let out = forward_fec(&red(FEC_PT, &fec), FEC_PT, &mapping).expect("forward");
            let header = parse_fec_header(&out[1..]).expect("header");
            assert_eq!(header.sn_base, 1_234);
            assert_eq!(header.long_mask, long_mask);
            // Nothing but the two SN base bytes differs.
            let mut expected = red(FEC_PT, &fec);
            expected[3..5].copy_from_slice(&1_234u16.to_be_bytes());
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn fec_sn_base_wraps_at_sixteen_bits() {
        let fec = fec_packet(65_530, false, b"");
        let up = FecMapping {
            seq_offset: 10,
            ..SAME_PT
        };
        let out = forward_fec(&red(FEC_PT, &fec), FEC_PT, &up).expect("forward");
        assert_eq!(parse_fec_header(&out[1..]).map(|h| h.sn_base), Some(4));

        // A downstream stream numbered below the publisher's: the offset is
        // the two's complement, and the base wraps the other way.
        let fec = fec_packet(3, false, b"");
        let down = FecMapping {
            seq_offset: 0u16.wrapping_sub(5),
            ..SAME_PT
        };
        let out = forward_fec(&red(FEC_PT, &fec), FEC_PT, &down).expect("forward");
        assert_eq!(parse_fec_header(&out[1..]).map(|h| h.sn_base), Some(65_534));
    }

    #[test]
    fn fec_is_dropped_when_the_timestamps_are_offset() {
        let fec = fec_packet(1, false, b"x");
        let mapping = FecMapping {
            time_offset: 1,
            ..SAME_PT
        };
        assert_eq!(
            forward_fec(&red(FEC_PT, &fec), FEC_PT, &mapping),
            Err(FecDrop::TimestampOffset)
        );
    }

    #[test]
    fn fec_is_dropped_when_the_media_payload_type_differs_or_is_unknown() {
        let fec = fec_packet(1, false, b"x");
        let differs = FecMapping {
            media_pt: Some((96, 100)),
            ..SAME_PT
        };
        assert_eq!(
            forward_fec(&red(FEC_PT, &fec), FEC_PT, &differs),
            Err(FecDrop::MediaPayloadTypeDiffers)
        );
        let unknown = FecMapping {
            media_pt: None,
            ..SAME_PT
        };
        assert_eq!(
            forward_fec(&red(FEC_PT, &fec), FEC_PT, &unknown),
            Err(FecDrop::MediaPayloadTypeUnknown)
        );
        // The subscriber's own ulpfec number may differ freely: it is only
        // the RED header byte.
        assert!(forward_fec(&red(FEC_PT, &fec), 99, &SAME_PT).is_ok());
        assert_eq!(
            forward_fec(&red(FEC_PT, &fec), 0x80, &SAME_PT),
            Err(FecDrop::BadPayloadType)
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Every one- and two-byte payload, and a sweep of longer ones.
        let mapping = FecMapping {
            seq_offset: 40_000,
            ..SAME_PT
        };
        for a in 0..=255u8 {
            let _ = red_block_pt(&[a]);
            let _ = red_rewrap(&[a], a & 0x7f);
            let _ = forward_fec(&[a], FEC_PT, &mapping);
            for b in [0u8, 0x40, 0x7f, 0x80, 0xff] {
                let _ = forward_fec(&[a, b], FEC_PT, &mapping);
                let _ = parse_fec_header(&[a, b]);
            }
        }
        for len in 0..40usize {
            for fill in [0u8, 0x40, 0xff] {
                let bytes = vec![fill; len];
                let _ = parse_fec_header(&bytes);
                let _ = forward_fec(&bytes, FEC_PT, &mapping);
                let _ = red_body(&bytes);
            }
        }
    }
}
