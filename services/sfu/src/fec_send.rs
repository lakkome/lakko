// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ULPFEC sender for one subscriber's video stream
//! ([ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4,
//! part 2).
//!
//! ## What it does
//!
//! A publisher's FEC packet cannot be forwarded: it covers the publisher's
//! header extension block, and every leg has its own (see
//! [`crate::video_fec`]). So the SFU computes a new FEC packet per
//! subscriber, over the packets as that subscriber receives them, and sends
//! it in the sequence-number slot the publisher's FEC packet left free on
//! that leg.
//!
//! A [`FecSender`] holds, for one downstream stream:
//!
//! - a bounded window of the media packets the stream sent, each as it was
//!   serialized for its **first** transmission, in the form a libwebrtc
//!   receiver feeds its FEC decoder ([`crate::fec_receive::media_from_red`]:
//!   RED header byte removed, the media payload type in the RTP header, the
//!   extensions a pacer fills in zeroed, payload and padding as sent);
//! - the requests that are waiting: "an FEC packet belongs at sequence number
//!   `slot` and protects these sequence numbers".
//!
//! A request is answered, with one FEC packet ([`encode`]), as soon as every
//! packet it protects has been sent. A request whose packets were not all
//! sent (another simulcast layer was on the wire, the subscriber was not
//! connected, the packet never reached the SFU) is never answered and is
//! dropped once the stream has moved [`REQUEST_DEADLINE`] sequence numbers
//! past its slot; the slot then stays a gap, as it was before this module.
//!
//! ## The packet built
//!
//! RFC 5109 with one protection level, as libwebrtc's
//! `ForwardErrorCorrection` writes and reads it: the 10-byte FEC header (`E`
//! = 0, `L`, the XOR of the protected packets' `P`, `X`, `CC`, `M` and
//! payload type, `SN base`, the XOR of their timestamps and of their
//! lengths), the protection length, a 16-bit or 48-bit mask, and the XOR of
//! everything after each protected packet's 12-byte fixed header. `SN base`
//! is the lowest protected sequence number (RFC 5109 section 7.3), so the
//! mask is the publisher's with any leading unset bits dropped, on the leg's
//! own sequence numbers.
//!
//! ## Opacity (invariant #2, C-39)
//!
//! A protected packet's payload is `SFrame` ciphertext. [`encode`] XORs whole
//! byte ranges together and never interprets one: the bytes it reads as
//! values are the first two bytes and the timestamp of the RTP fixed header,
//! and each packet's length. What a subscriber rebuilds from the result is
//! the ciphertext the publisher sealed.
//!
//! ## Bounds
//!
//! Per stream at most [`MEDIA_WINDOW`] media packets and [`MAX_REQUESTS`]
//! requests are held, no packet longer than
//! [`MAX_PACKET_LEN`](crate::fec_receive::MAX_PACKET_LEN). Nothing panics on
//! any input.
//!
//! Pure: no I/O, no clock.

use std::collections::VecDeque;

use crate::fec_receive::{MAX_PACKET_LEN, MAX_PROTECTED, RTP_HEADER_LEN};

/// Sequence numbers behind the newest sent one that the window covers, and
/// so the most media packets held per stream.
pub const MEDIA_WINDOW: u16 = 128;

/// Most requests waiting per stream. One more drops the oldest.
pub const MAX_REQUESTS: usize = 48;

/// A request is dropped once the stream has sent a packet this many sequence
/// numbers past the request's slot. Chosen, not measured: a libwebrtc
/// receiver holds the last 48 media packets for its FEC decoder, so an FEC
/// packet arriving later than that can repair nothing.
pub const REQUEST_DEADLINE: u16 = 64;

/// Longest FEC packet built (FEC header, mask and protected bytes). With the
/// RED header byte, the RTP headers, SRTP padding and the SRTP tag the
/// datagram stays under 1500 bytes, the longest packet a libwebrtc receiver
/// accepts.
pub const MAX_FEC_LEN: usize = 1400;

/// Offset of the mask in an FEC packet: after the 10-byte FEC header and the
/// 2-byte protection length.
const FEC_MASK_OFFSET: usize = 12;

/// `L` bit of the FEC header: the mask is 48 bits rather than 16.
const FEC_LONG_MASK_BIT: u8 = 0x40;

/// Bits of the short mask.
const SHORT_MASK_BITS: u16 = 16;

/// `SN base`, the mask and whether it is the long one, for the protected
/// sequence numbers `seqs`.
///
/// `seqs` must be in sending order: each later than the one before it,
/// modulo 2^16, and all within the 48 numbers a long mask reaches from the
/// first. `None` for an empty list, more than 48 numbers, a repeated or
/// descending number, or a span no mask covers.
#[must_use]
pub fn mask_for(seqs: &[u16]) -> Option<(u16, [u8; 6], bool)> {
    let base = *seqs.first()?;
    if seqs.len() > MAX_PROTECTED {
        return None;
    }
    let reach = u16::try_from(MAX_PROTECTED).ok()?;
    let mut mask = [0u8; 6];
    let mut last: Option<u16> = None;
    for seq in seqs {
        let distance = seq.wrapping_sub(base);
        if distance >= reach || last.is_some_and(|l| distance <= l) {
            return None;
        }
        last = Some(distance);
        let byte = mask.get_mut(usize::from(distance / 8))?;
        *byte |= 0x80 >> (distance % 8);
    }
    let long = last.is_some_and(|l| l >= SHORT_MASK_BITS);
    Some((base, mask, long))
}

/// One FEC packet (what follows the RED header byte) over `packets`.
///
/// Each entry is a sequence number and the whole RTP packet sent under it,
/// in the form described in the module doc; the entries must be in sending
/// order (see [`mask_for`]). `None` when the list is not one a mask can
/// name, a packet is shorter than an RTP header or longer than
/// [`MAX_PACKET_LEN`], or the result would be longer than [`MAX_FEC_LEN`].
///
/// **Opacity (C-39).** Everything after a packet's fixed header, the
/// `SFrame` ciphertext included, is XORed into the result and never read.
#[must_use]
pub fn encode(packets: &[(u16, &[u8])]) -> Option<Vec<u8>> {
    let seqs: Vec<u16> = packets.iter().map(|(seq, _)| *seq).collect();
    let (sn_base, mask, long) = mask_for(&seqs)?;
    let mask_len = if long { 6 } else { 2 };
    let header_len = FEC_MASK_OFFSET + mask_len;
    let mut longest = 0usize;
    for (_, packet) in packets {
        if packet.len() < RTP_HEADER_LEN || packet.len() > MAX_PACKET_LEN {
            return None;
        }
        longest = longest.max(packet.len() - RTP_HEADER_LEN);
    }
    if header_len + longest > MAX_FEC_LEN {
        return None;
    }
    let mut head = [0u8; 2];
    let mut ts = [0u8; 4];
    let mut length: u16 = 0;
    let mut body = vec![0u8; longest];
    for (_, packet) in packets {
        let fixed = packet.get(..RTP_HEADER_LEN)?;
        let tail = packet.get(RTP_HEADER_LEN..)?;
        head[0] ^= fixed[0];
        head[1] ^= fixed[1];
        for (t, b) in ts.iter_mut().zip(&fixed[4..8]) {
            *t ^= b;
        }
        length ^= u16::try_from(tail.len()).ok()?;
        // Header extension block, payload (ciphertext) and padding: combined,
        // not read.
        for (x, b) in body.iter_mut().zip(tail) {
            *x ^= b;
        }
    }
    let mut fec = Vec::with_capacity(header_len + longest);
    // E = 0; L says which mask follows; the low six bits are the XOR of the
    // protected packets' P, X and CC.
    fec.push((head[0] & 0x3f) | if long { FEC_LONG_MASK_BIT } else { 0 });
    fec.push(head[1]);
    fec.extend_from_slice(&sn_base.to_be_bytes());
    fec.extend_from_slice(&ts);
    fec.extend_from_slice(&length.to_be_bytes());
    fec.extend_from_slice(&u16::try_from(longest).ok()?.to_be_bytes());
    fec.extend_from_slice(mask.get(..mask_len)?);
    fec.extend_from_slice(&body);
    Some(fec)
}

/// Why a request was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// The protected sequence numbers are not a list a mask can name (see
    /// [`mask_for`]).
    Unmaskable,
    /// The slot is one of the protected numbers, or a request for it is
    /// already waiting.
    SlotTaken,
    /// The stream has already moved too far past the slot.
    TooLate,
}

/// One FEC packet ready to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated<T> {
    /// Sequence number it is sent under.
    pub slot: u16,
    /// What the caller attached to the request.
    pub tag: T,
    /// The FEC packet: what follows the RED header byte.
    pub fec: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request<T> {
    slot: u16,
    protected: Vec<u16>,
    tag: T,
}

/// ULPFEC send state for one downstream stream. `T` is whatever the caller
/// needs back with a generated packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FecSender<T> {
    /// Sent media packets by sequence number modulo the window. A slot holds
    /// a packet only while its sequence number is within the window.
    media: Vec<Option<(u16, Vec<u8>)>>,
    requests: VecDeque<Request<T>>,
    /// The newest sequence number sent.
    newest: Option<u16>,
}

impl<T> Default for FecSender<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> FecSender<T> {
    /// A sender holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            media: (0..MEDIA_WINDOW).map(|_| None).collect(),
            requests: VecDeque::new(),
            newest: None,
        }
    }

    /// Media packets held now. At most [`MEDIA_WINDOW`].
    #[must_use]
    pub fn media_held(&self) -> usize {
        self.media.iter().flatten().count()
    }

    /// Requests waiting now. At most [`MAX_REQUESTS`].
    #[must_use]
    pub fn requests_waiting(&self) -> usize {
        self.requests.len()
    }

    fn index(seq: u16) -> usize {
        usize::from(seq % MEDIA_WINDOW)
    }

    fn packet(&self, seq: u16) -> Option<&[u8]> {
        match self.media.get(Self::index(seq)) {
            Some(Some((held, packet))) if *held == seq => Some(packet),
            _ => None,
        }
    }

    /// Move the window to cover `seq`. `false` when `seq` is older than the
    /// window.
    fn admit(&mut self, seq: u16) -> bool {
        let Some(newest) = self.newest else {
            self.newest = Some(seq);
            return true;
        };
        let ahead = seq.wrapping_sub(newest);
        if ahead < 0x8000 {
            if ahead >= MEDIA_WINDOW {
                for held in &mut self.media {
                    *held = None;
                }
            } else {
                for step in 1..=ahead {
                    let index = Self::index(newest.wrapping_add(step));
                    if let Some(held) = self.media.get_mut(index) {
                        *held = None;
                    }
                }
            }
            self.newest = Some(seq);
            self.requests
                .retain(|r| seq.wrapping_sub(r.slot) < REQUEST_DEADLINE || past(r.slot, seq));
            return true;
        }
        newest.wrapping_sub(seq) < MEDIA_WINDOW
    }

    /// Record a media packet the stream sent under `seq`, in the form
    /// described in the module doc, and return every FEC packet that can now
    /// be built. A packet older than the window, one that is not RTP or is
    /// too long, and a second packet under a number already held are
    /// ignored.
    pub fn sent_media(&mut self, seq: u16, packet: Vec<u8>) -> Vec<Generated<T>> {
        if packet.len() < RTP_HEADER_LEN || packet.len() > MAX_PACKET_LEN {
            return Vec::new();
        }
        if !self.admit(seq) || self.packet(seq).is_some() {
            return Vec::new();
        }
        if let Some(held) = self.media.get_mut(Self::index(seq)) {
            *held = Some((seq, packet));
        }
        self.generate()
    }

    /// Ask for an FEC packet at sequence number `slot` over the packets sent
    /// under `protected` (in sending order). Returns it at once when all of
    /// them were sent already; otherwise it follows from a later
    /// [`Self::sent_media`], or never.
    ///
    /// # Errors
    ///
    /// A [`RequestError`]; nothing is stored then.
    pub fn request(
        &mut self,
        slot: u16,
        protected: Vec<u16>,
        tag: T,
    ) -> Result<Vec<Generated<T>>, RequestError> {
        if mask_for(&protected).is_none() {
            return Err(RequestError::Unmaskable);
        }
        if protected.contains(&slot) || self.requests.iter().any(|r| r.slot == slot) {
            return Err(RequestError::SlotTaken);
        }
        if let Some(newest) = self.newest {
            if !past(slot, newest) && newest.wrapping_sub(slot) >= REQUEST_DEADLINE {
                return Err(RequestError::TooLate);
            }
        }
        if self.requests.len() >= MAX_REQUESTS {
            self.requests.pop_front();
        }
        self.requests.push_back(Request {
            slot,
            protected,
            tag,
        });
        Ok(self.generate())
    }

    /// Answer every request whose packets are all held.
    fn generate(&mut self) -> Vec<Generated<T>> {
        let mut out = Vec::new();
        let mut idx = 0;
        while idx < self.requests.len() {
            let complete = self
                .requests
                .get(idx)
                .is_some_and(|r| r.protected.iter().all(|seq| self.packet(*seq).is_some()));
            if !complete {
                idx += 1;
                continue;
            }
            let Some(request) = self.requests.remove(idx) else {
                break;
            };
            let packets: Option<Vec<(u16, &[u8])>> = request
                .protected
                .iter()
                .map(|seq| self.packet(*seq).map(|p| (*seq, p)))
                .collect();
            // A set that cannot be encoded (too long a packet in it) is
            // dropped with its request: it would not encode later either.
            if let Some(fec) = packets.as_deref().and_then(encode) {
                out.push(Generated {
                    slot: request.slot,
                    tag: request.tag,
                    fec,
                });
            }
        }
        out
    }
}

/// Whether `slot` lies ahead of `newest` (it was not reached yet).
const fn past(slot: u16, newest: u16) -> bool {
    let ahead = slot.wrapping_sub(newest);
    ahead != 0 && ahead < 0x8000
}

#[cfg(test)]
// Test data is generated from counters; the low bits are the point.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::fec_receive::{FecReceiver, Recovered};

    const SSRC: u32 = 0x5566_7788;
    const VP8: u8 = 100;

    /// A media packet in the form the encoder covers: an extension block
    /// (zeroed abs-send-time and transport-cc, a `mid`), then `payload`.
    fn media(seq: u16, time: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x90, (u8::from(marker) << 7) | VP8];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&time.to_be_bytes());
        p.extend_from_slice(&SSRC.to_be_bytes());
        p.extend_from_slice(&[0xbe, 0xde, 0x00, 0x03]);
        p.extend_from_slice(&[0x22, 0, 0, 0, 0x31, 0, 0, 0x40, b'7', 0, 0, 0]);
        p.extend_from_slice(payload);
        p
    }

    /// `n` media packets from `first`, of differing lengths and timestamps
    /// that rise with the sequence numbers.
    fn group(first: u16, n: u16) -> Vec<(u16, Vec<u8>)> {
        (0..n)
            .map(|i| {
                let seq = first.wrapping_add(i);
                let payload: Vec<u8> = (0..17 + usize::from(i) * 11)
                    .map(|b| (b as u8).wrapping_mul(29).wrapping_add(seq as u8))
                    .collect();
                (
                    seq,
                    media(seq, 9_000 + u32::from(i / 3) * 3_000, i % 3 == 2, &payload),
                )
            })
            .collect()
    }

    fn refs(packets: &[(u16, Vec<u8>)]) -> Vec<(u16, &[u8])> {
        packets.iter().map(|(s, p)| (*s, p.as_slice())).collect()
    }

    /// Decode with the receiver of [`crate::fec_receive`] (written from
    /// RFC 5109 section 9.2 and libwebrtc): every protected packet, taken out
    /// one at a time, comes back byte for byte.
    fn each_packet_is_recoverable(packets: &[(u16, Vec<u8>)], fec_seq: u16, fec: &[u8]) {
        for lost in 0..packets.len() {
            let mut rx = FecReceiver::new(SSRC);
            for (i, (seq, p)) in packets.iter().enumerate() {
                if i != lost {
                    assert!(rx.insert_media(*seq, p.clone()).is_empty());
                }
            }
            let got = rx.insert_fec(fec_seq, fec).expect("taken");
            assert_eq!(
                got,
                vec![Recovered {
                    seq: packets[lost].0,
                    packet: packets[lost].1.clone()
                }],
                "lost={lost}"
            );
        }
    }

    #[test]
    fn the_mask_starts_at_the_first_protected_number() {
        assert_eq!(
            mask_for(&[10, 11, 13]),
            Some((10, [0b1101_0000, 0, 0, 0, 0, 0], false))
        );
        // The sixteenth number still fits the short mask, the seventeenth
        // needs the long one.
        assert_eq!(
            mask_for(&[10, 25]),
            Some((10, [0x80, 0x01, 0, 0, 0, 0], false))
        );
        assert_eq!(
            mask_for(&[10, 26]),
            Some((10, [0x80, 0, 0x80, 0, 0, 0], true))
        );
        assert_eq!(
            mask_for(&[10, 57]),
            Some((10, [0x80, 0, 0, 0, 0, 0x01], true))
        );
    }

    #[test]
    fn the_mask_follows_the_numbers_across_the_wrap() {
        assert_eq!(
            mask_for(&[65_534, 65_535, 0, 1]),
            Some((65_534, [0xf0, 0, 0, 0, 0, 0], false))
        );
        assert_eq!(
            mask_for(&[65_535, 0]),
            Some((65_535, [0xc0, 0, 0, 0, 0, 0], false))
        );
        assert_eq!(
            mask_for(&[65_520, 0]),
            Some((65_520, [0x80, 0, 0x80, 0, 0, 0], true))
        );
    }

    #[test]
    fn lists_no_mask_can_name_are_refused() {
        assert_eq!(mask_for(&[]), None);
        assert_eq!(mask_for(&[10, 58]), None, "past the 48th number");
        assert_eq!(mask_for(&[10, 9]), None, "descending");
        assert_eq!(mask_for(&[10, 10]), None, "repeated");
        assert_eq!(mask_for(&[10, 12, 11]), None, "out of order");
        let many: Vec<u16> = (0..49).collect();
        assert_eq!(mask_for(&many), None, "more than 48");
        let all: Vec<u16> = (0..48).collect();
        assert_eq!(mask_for(&all), Some((0, [0xff; 6], true)));
    }

    #[test]
    fn every_protected_packet_is_recoverable_from_the_built_packet() {
        for (first, n) in [
            (1000u16, 1u16),
            (1000, 3),
            (1000, 16),
            (1000, 17),
            (300, 48),
        ] {
            let packets = group(first, n);
            let fec = encode(&refs(&packets)).expect("encodes");
            assert_eq!(fec[0] & 0x80, 0, "E is zero");
            assert_eq!(fec[0] & 0x40 != 0, n > 16, "L follows the span");
            assert_eq!(u16::from_be_bytes([fec[2], fec[3]]), first, "SN base");
            each_packet_is_recoverable(&packets, first.wrapping_add(n), &fec);
        }
    }

    #[test]
    fn a_sparse_set_and_a_set_across_the_wrap_are_recoverable() {
        // Every other packet of a run, as a publisher's interleaved mask
        // protects them.
        let all = group(65_530, 12);
        let sparse: Vec<(u16, Vec<u8>)> = all.iter().step_by(2).cloned().collect();
        assert_eq!(sparse[3].0, 0, "the set crosses the wrap");
        let fec = encode(&refs(&sparse)).expect("encodes");
        assert_eq!(u16::from_be_bytes([fec[2], fec[3]]), 65_530);
        assert_eq!(&fec[12..14], &[0b1010_1010, 0b1010_0000]);
        each_packet_is_recoverable(&sparse, 6, &fec);
    }

    #[test]
    fn the_length_and_the_header_fields_of_a_rebuilt_packet_are_the_originals() {
        // Packets of very different lengths, one with the marker bit and its
        // own timestamp, one with RTP padding.
        let mut padded = media(502, 12_000, true, b"short");
        padded[0] |= 0x20;
        padded.extend_from_slice(&[0, 0, 3]);
        let packets = vec![
            (500u16, media(500, 9_000, false, &[7u8; 900])),
            (501, media(501, 9_000, false, b"x")),
            (502, padded),
        ];
        let fec = encode(&refs(&packets)).expect("encodes");
        assert_eq!(
            usize::from(u16::from_be_bytes([fec[10], fec[11]])),
            packets[0].1.len() - RTP_HEADER_LEN,
            "the protection length is the longest protected tail"
        );
        assert_eq!(fec.len(), 14 + packets[0].1.len() - RTP_HEADER_LEN);
        each_packet_is_recoverable(&packets, 503, &fec);
    }

    #[test]
    fn packets_that_cannot_be_protected_are_refused() {
        let ok = media(1, 1, false, b"fine");
        let short = vec![0x80u8; RTP_HEADER_LEN - 1];
        let long = vec![0x80u8; MAX_PACKET_LEN + 1];
        assert!(encode(&[]).is_none());
        assert!(encode(&[(1, ok.as_slice()), (2, short.as_slice())]).is_none());
        assert!(encode(&[(1, ok.as_slice()), (2, long.as_slice())]).is_none());
        // Within the packet bound, past the FEC bound.
        let big = media(2, 1, false, &[1u8; MAX_FEC_LEN]);
        assert!(big.len() <= MAX_PACKET_LEN);
        assert!(encode(&[(1, ok.as_slice()), (2, big.as_slice())]).is_none());
        // The longest that still fits.
        let fits = vec![0x80u8; RTP_HEADER_LEN + MAX_FEC_LEN - 14];
        let fec = encode(&[(1, fits.as_slice())]).expect("fits");
        assert_eq!(fec.len(), MAX_FEC_LEN);
    }

    #[test]
    fn a_request_is_answered_when_its_last_packet_is_sent() {
        let packets = group(40, 3);
        let mut tx = FecSender::new();
        assert!(tx.sent_media(40, packets[0].1.clone()).is_empty());
        assert_eq!(tx.request(43, vec![40, 41, 42], "tag"), Ok(Vec::new()));
        assert!(tx.sent_media(42, packets[2].1.clone()).is_empty());
        assert_eq!(tx.requests_waiting(), 1);
        let got = tx.sent_media(41, packets[1].1.clone());
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].slot, got[0].tag), (43, "tag"));
        assert_eq!(Some(&got[0].fec), encode(&refs(&packets)).as_ref());
        assert_eq!(tx.requests_waiting(), 0, "answered once");
        // The packets again change nothing.
        assert!(tx.sent_media(41, packets[1].1.clone()).is_empty());
    }

    #[test]
    fn a_request_whose_packets_were_all_sent_is_answered_at_once() {
        let packets = group(65_534, 3);
        let mut tx = FecSender::new();
        for (seq, p) in &packets {
            assert!(tx.sent_media(*seq, p.clone()).is_empty());
        }
        let got = tx.request(1, vec![65_534, 65_535, 0], 7u8).expect("taken");
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].slot, got[0].tag), (1, 7));
        each_packet_is_recoverable(&packets, 1, &got[0].fec);
    }

    #[test]
    fn a_request_with_a_packet_never_sent_is_dropped_and_builds_nothing() {
        let mut tx = FecSender::new();
        let packets = group(100, 3);
        let _ = tx.sent_media(100, packets[0].1.clone());
        let _ = tx.sent_media(102, packets[2].1.clone());
        assert_eq!(tx.request(103, vec![100, 101, 102], ()), Ok(Vec::new()));
        // The stream moves on without 101.
        for seq in 104..103 + REQUEST_DEADLINE {
            assert!(tx
                .sent_media(seq, media(seq, 1, false, b"later"))
                .is_empty());
            assert_eq!(tx.requests_waiting(), 1, "seq={seq}");
        }
        let at = 103 + REQUEST_DEADLINE;
        assert!(tx.sent_media(at, media(at, 1, false, b"later")).is_empty());
        assert_eq!(tx.requests_waiting(), 0, "dropped at the deadline");
        // 101 arriving now (a very late repair) builds nothing.
        assert!(tx.sent_media(101, packets[1].1.clone()).is_empty());
        // And a request that far behind is not taken at all.
        assert_eq!(
            tx.request(103, vec![100, 101, 102], ()),
            Err(RequestError::TooLate)
        );
    }

    #[test]
    fn requests_that_make_no_sense_are_refused() {
        let mut tx: FecSender<()> = FecSender::new();
        assert_eq!(tx.request(5, vec![], ()), Err(RequestError::Unmaskable));
        assert_eq!(tx.request(5, vec![3, 2], ()), Err(RequestError::Unmaskable));
        assert_eq!(tx.request(3, vec![2, 3], ()), Err(RequestError::SlotTaken));
        assert_eq!(tx.request(5, vec![2, 3], ()), Ok(Vec::new()));
        assert_eq!(tx.request(5, vec![2, 4], ()), Err(RequestError::SlotTaken));
        assert_eq!(tx.requests_waiting(), 1);
    }

    #[test]
    fn a_slot_ahead_of_everything_sent_is_not_too_late() {
        let mut tx = FecSender::new();
        let _ = tx.sent_media(65_530, media(65_530, 1, false, b"a"));
        // The slot lies across the wrap, ahead of the newest packet.
        assert_eq!(tx.request(2, vec![65_530, 0], ()), Ok(Vec::new()));
        let _ = tx.sent_media(1, media(1, 1, false, b"c"));
        assert_eq!(tx.requests_waiting(), 1, "a slot not reached yet is kept");
        let got = tx.sent_media(0, media(0, 1, false, b"b"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].slot, 2);
    }

    #[test]
    fn a_stale_slot_is_never_read_as_the_packet_a_window_later() {
        let mut tx = FecSender::new();
        let _ = tx.sent_media(7, media(7, 1, false, b"old"));
        for seq in [60, 120] {
            let _ = tx.sent_media(seq, media(seq, 1, false, b"between"));
        }
        let later = 7 + MEDIA_WINDOW;
        // 7 + window is asked for; the packet numbered 7 must not stand in.
        assert_eq!(
            tx.request(later + 2, vec![later, later + 1], ()),
            Ok(Vec::new())
        );
        let got = tx.sent_media(later + 1, media(later + 1, 1, false, b"b"));
        assert!(got.is_empty(), "the first of the two was never sent");
        assert_eq!(tx.media_held(), 3, "7 left the window");
    }

    #[test]
    fn the_bounds_hold_and_nothing_panics_on_hostile_input() {
        let mut tx: FecSender<u32> = FecSender::new();
        let mut state: u32 = 0x2545_f491;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..30_000u32 {
            let seq = (next() % 700) as u16;
            let len = (next() % 90) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            for g in tx.sent_media(seq, bytes) {
                assert!(g.fec.len() <= MAX_FEC_LEN);
            }
            let count = (next() % 6) as usize;
            let protected: Vec<u16> = (0..count).map(|_| (next() % 700) as u16).collect();
            let _ = mask_for(&protected);
            if let Ok(generated) = tx.request((next() % 700) as u16, protected, round) {
                for g in generated {
                    assert!(g.fec.len() >= 14 && g.fec.len() <= MAX_FEC_LEN);
                }
            }
            assert!(tx.media_held() <= usize::from(MEDIA_WINDOW));
            assert!(tx.requests_waiting() <= MAX_REQUESTS);
        }
        // Oversized media is not stored.
        let mut tx: FecSender<()> = FecSender::new();
        assert!(tx.sent_media(1, vec![0x80; MAX_PACKET_LEN + 1]).is_empty());
        assert_eq!(tx.media_held(), 0);
        // More requests than the bound: the oldest go.
        for slot in 0..(MAX_REQUESTS as u16 + 10) {
            let base = 1_000 + slot * 4;
            let _ = tx.request(base + 3, vec![base, base + 1], ());
        }
        assert_eq!(tx.requests_waiting(), MAX_REQUESTS);
    }
}
