// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The RED header of a video packet
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
//!   as it would have been sent without RED (for Lakko, `SFrame` ciphertext);
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
//! - **FEC**: forwarded to nobody. A publisher's FEC is computed over its
//!   packets as the publisher sent them, header extension block included
//!   (RFC 5109 section 7.3; libwebrtc `forward_error_correction.cc`,
//!   `XorPayloads` reads from byte 12). The SFU does not forward a
//!   publisher's extension block: each leg's block is written by `str0m`
//!   with that leg's own `mid`, extension ids and timing of when `mid` is
//!   present. A subscriber that used a publisher's FEC packet would rebuild
//!   a packet with a wrong extension block, length or payload. So the FEC is
//!   used where it is valid, on the publisher's leg: [`crate::fec_receive`]
//!   rebuilds media packets the SFU did not receive, and those are forwarded
//!   as RED media like any other.
//!
//! A subscriber that negotiated the pair receives RED-wrapped media and, in
//! the sequence-number slot of each publisher FEC packet, an FEC packet the
//! SFU computed over the same media packets as that subscriber was sent them
//! ([`crate::fec_send`]). When one of those packets was not sent to the
//! subscriber, no FEC packet is sent and the slot is a gap.
//!
//! ## Opacity (invariant #2, C-39)
//!
//! What is read here is one byte of cleartext transport framing that sits
//! outside the `SFrame`-sealed frame: the RED header. No function in this
//! module reads, compares or alters a byte of a media body; it is copied.
//!
//! Pure functions: no I/O, no state, no panic on any input.

/// Length of the RED header for a single-block packet.
pub const RED_HEADER_LEN: usize = 1;

/// `F` bit of a RED block header: set when another block header follows.
const RED_FOLLOW_BIT: u8 = 0x80;

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

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIA_PT: u8 = 96;
    const FEC_PT: u8 = 118;

    fn red(block_pt: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![block_pt];
        p.extend_from_slice(body);
        p
    }

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

        // One byte: a header and an empty body.
        assert_eq!(red_block_pt(&[MEDIA_PT]), Ok(MEDIA_PT));
        assert_eq!(red_body(&[MEDIA_PT]), Some(&[][..]));
        assert_eq!(red_rewrap(&[MEDIA_PT], 100), Some(vec![100]));
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
    fn arbitrary_bytes_never_panic() {
        for a in 0..=255u8 {
            let _ = red_block_pt(&[a]);
            let _ = red_rewrap(&[a], a & 0x7f);
            let _ = red_body(&[a, a]);
        }
    }
}
