// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ULPFEC receiver for a publisher's video stream
//! ([ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md) step 4,
//! part 1).
//!
//! ## What it does
//!
//! A libwebrtc publisher that negotiated `red` and `ulpfec` sends, on one
//! SSRC and in one sequence-number space, its media packets and FEC packets
//! (RFC 5109), each inside a one-byte RED header (see [`crate::video_fec`]).
//! An FEC packet is the XOR of up to 48 media packets and can rebuild one of
//! them when it is the only one missing.
//!
//! On the publisher's leg the SFU holds every packet byte for byte as the
//! publisher sent it, so the publisher's FEC is valid there. A
//! [`FecReceiver`] keeps a bounded window of recent media packets and of FEC
//! packets that cannot be used yet, and on every arrival rebuilds what it
//! can. A rebuilt packet is handed back as a whole RTP packet; the caller
//! (`MediaPeer`) registers it with `str0m` as received and forwards it to
//! subscribers like any other packet of the stream.
//!
//! ## The form packets are held in
//!
//! libwebrtc computes FEC over the media packet as it is before RED wrapping
//! and before the pacer fills in the header extensions it calls mutable
//! (`rtp_sender_egress.cc`; `RtpPacket::ZeroMutableExtensions`). The receiver
//! therefore holds each media packet in that form ([`media_from_red`]):
//!
//! - the RED header byte is removed and the payload type in the RTP header is
//!   the media payload type the RED header named;
//! - the absolute send time, transport-wide sequence number and transmission
//!   offset extensions are zeroed, and so is the tail of the video-timing
//!   extension; every other extension (`mid`, the frame descriptor, video
//!   orientation) is left as sent;
//! - CSRC list, payload and padding are untouched.
//!
//! FEC covers the first two header bytes, the timestamp, the length, and
//! every byte after the 12-byte fixed header (RFC 5109 section 7.3; libwebrtc
//! `forward_error_correction.cc`, `XorHeaders` and `XorPayloads`). A packet
//! that reached the SFU as a retransmission is never given to the receiver:
//! an RTX packet's extension block is its own, not the original's, and using
//! it would rebuild garbage (libwebrtc's `UlpfecReceiver` skips such packets
//! for the same reason).
//!
//! ## Opacity (invariant #2, C-39)
//!
//! The media payload is `SFrame` ciphertext and stays so. This module XORs
//! whole byte ranges together and never interprets a payload byte: the bytes
//! it reads are the RTP fixed header, the header-extension framing (ids and
//! lengths, to find the extensions to zero), the RED header byte and the FEC
//! header. A rebuilt packet's payload is the ciphertext the publisher sealed,
//! or, when the inputs were not what the FEC was computed over, bytes that
//! fail `SFrame` authentication at the subscriber. The SFU cannot tell the two
//! apart and does not try.
//!
//! ## Bounds
//!
//! Every byte handled here is chosen by an admitted peer. Nothing panics on
//! any input, and per stream the receiver holds at most [`MEDIA_WINDOW`]
//! media packets and [`MAX_FEC_PACKETS`] FEC packets, none longer than
//! [`MAX_PACKET_LEN`]. Work per arrival is bounded by
//! `MAX_FEC_PACKETS * MAX_FEC_PACKETS * MAX_PROTECTED` window lookups plus one
//! XOR pass per rebuilt packet.
//!
//! Pure: no I/O, no clock.

use std::collections::VecDeque;

/// Length of the RTP fixed header. FEC covers bytes 0, 1 and 4..8 of it and
/// everything after it.
pub const RTP_HEADER_LEN: usize = 12;

/// Longest packet held or rebuilt (libwebrtc's `IP_PACKET_SIZE`). A longer
/// media or FEC packet is not stored.
pub const MAX_PACKET_LEN: usize = 1500;

/// Sequence numbers behind the newest one that the window covers, and so the
/// most media packets held per stream.
pub const MEDIA_WINDOW: u16 = 256;

/// Most FEC packets held per stream while they wait for more media
/// (libwebrtc holds 48).
pub const MAX_FEC_PACKETS: usize = 48;

/// Most media packets one FEC packet protects: the width of the long mask.
pub const MAX_PROTECTED: usize = 48;

/// Consecutive packets older than the window after which the window starts
/// over at the packet that arrived: the stream restarted its numbering.
const STALE_RESET: u32 = 32;

/// `L` bit of the FEC header: the mask is 48 bits rather than 16.
const FEC_LONG_MASK_BIT: u8 = 0x40;

/// Offset of the mask in an FEC packet: after the 10-byte FEC header and the
/// 2-byte protection length.
const FEC_MASK_OFFSET: usize = 12;

/// Offset within the video-timing extension from which libwebrtc zeroes
/// (`VideoTimingExtension::kPacerExitDeltaOffset`).
const VIDEO_TIMING_PACER_EXIT_OFFSET: usize = 7;

/// The header extension ids, on one publisher's media line, of the
/// extensions a libwebrtc sender fills in after computing FEC. `None` when
/// the extension was not negotiated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MutableExtensions {
    /// `abs-send-time`: zeroed whole.
    pub abs_send_time: Option<u8>,
    /// `transport-wide-cc`: zeroed whole.
    pub transport_cc: Option<u8>,
    /// `toffset` (transmission time offset): zeroed whole.
    pub transmission_offset: Option<u8>,
    /// `video-timing`: zeroed from byte 7 on.
    pub video_timing: Option<u8>,
}

impl MutableExtensions {
    /// From which byte of the extension with `id` to zero, if at all.
    fn zero_from(self, id: u8) -> Option<usize> {
        let whole = [
            self.abs_send_time,
            self.transport_cc,
            self.transmission_offset,
        ];
        if whole.contains(&Some(id)) {
            Some(0)
        } else if self.video_timing == Some(id) {
            Some(VIDEO_TIMING_PACER_EXIT_OFFSET)
        } else {
            None
        }
    }
}

/// Length of an RTP packet's headers: fixed header, CSRC list and header
/// extension block. `None` when the packet is not RTP version 2 or is
/// shorter than the headers it announces.
#[must_use]
pub fn rtp_headers_len(packet: &[u8]) -> Option<usize> {
    let first = *packet.first()?;
    if packet.len() < RTP_HEADER_LEN || first >> 6 != 2 {
        return None;
    }
    let mut len = RTP_HEADER_LEN + 4 * usize::from(first & 0x0f);
    if first & 0x10 != 0 {
        let words = packet.get(len + 2..len + 4)?;
        len += 4 + 4 * usize::from(u16::from_be_bytes([words[0], words[1]]));
    }
    (packet.len() >= len).then_some(len)
}

/// Zero, in place, the extensions of `packet` named by `mutable`. Reads the
/// extension framing (RFC 8285 one-byte and two-byte forms) and nothing of
/// the payload. A packet without an extension block, or with a block in a
/// form this does not know, is left as it is.
pub fn zero_mutable_extensions(packet: &mut [u8], mutable: &MutableExtensions) {
    let Some(first) = packet.first().copied() else {
        return;
    };
    if first & 0x10 == 0 {
        return;
    }
    let Some(headers_len) = rtp_headers_len(packet) else {
        return;
    };
    let block_start = RTP_HEADER_LEN + 4 * usize::from(first & 0x0f);
    let Some(profile) = packet.get(block_start..block_start + 2) else {
        return;
    };
    let profile = u16::from_be_bytes([profile[0], profile[1]]);
    let two_byte = if profile == 0xbede {
        false
    } else if profile & 0xfff0 == 0x1000 {
        true
    } else {
        return;
    };
    let mut at = block_start + 4;
    while at < headers_len {
        let Some(&head) = packet.get(at) else {
            return;
        };
        if head == 0 {
            // Padding byte.
            at += 1;
            continue;
        }
        let (id, len, data_at) = if two_byte {
            let Some(&len) = packet.get(at + 1) else {
                return;
            };
            (head, usize::from(len), at + 2)
        } else {
            if head >> 4 == 15 {
                // Reserved id: nothing after it is an extension.
                return;
            }
            (head >> 4, usize::from(head & 0x0f) + 1, at + 1)
        };
        let end = data_at + len;
        if end > headers_len {
            return;
        }
        if let Some(from) = mutable.zero_from(id) {
            if let Some(bytes) = packet.get_mut(data_at + from.min(len)..end) {
                bytes.fill(0);
            }
        }
        at = end;
    }
}

/// The media packet inside a RED-wrapped RTP packet, in the form the
/// publisher's FEC was computed over (see the module doc).
///
/// `received` is the whole RTP packet as it came off the wire after SRTP
/// decryption. `None` when it is not RTP, has no payload, announces a
/// further RED block, or is longer than [`MAX_PACKET_LEN`].
#[must_use]
pub fn media_from_red(received: &[u8], mutable: &MutableExtensions) -> Option<Vec<u8>> {
    let headers_len = rtp_headers_len(received)?;
    let red_header = *received.get(headers_len)?;
    if red_header & 0x80 != 0 || received.len() - 1 > MAX_PACKET_LEN {
        return None;
    }
    let mut packet = Vec::with_capacity(received.len() - 1);
    packet.extend_from_slice(received.get(..headers_len)?);
    packet.extend_from_slice(received.get(headers_len + 1..)?);
    // Keep the marker bit, name the media payload type.
    if let Some(second) = packet.get_mut(1) {
        *second = (*second & 0x80) | red_header;
    }
    zero_mutable_extensions(&mut packet, mutable);
    Some(packet)
}

/// Why an FEC packet was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FecError {
    /// Shorter than the headers its own `L` bit announces.
    Truncated,
    /// Shorter than its headers plus the protection length it states.
    Inconsistent,
    /// Longer than [`MAX_PACKET_LEN`], or states a protection length no
    /// packet could have.
    Oversized,
    /// Its mask names no packet.
    ProtectsNothing,
    /// Too old for the window, or a packet with this sequence number is
    /// already held.
    NotNeeded,
}

/// One media packet rebuilt from FEC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// Its sequence number.
    pub seq: u16,
    /// The whole RTP packet in the form described in the module doc: media
    /// payload type, no RED header byte, mutable extensions zero.
    pub packet: Vec<u8>,
}

/// One FEC packet waiting for media.
#[derive(Debug, Clone)]
struct FecPacket {
    seq: u16,
    sn_base: u16,
    /// Sequence numbers the mask names, in mask order.
    protected: Vec<u16>,
    /// First two bytes: `E`, `L`, then the XOR of the protected packets'
    /// `P`, `X`, `CC`, `M` and payload type.
    head: [u8; 2],
    /// XOR of the protected packets' timestamps.
    ts_recovery: [u8; 4],
    /// XOR of the protected packets' lengths after the fixed header.
    length_recovery: u16,
    /// XOR of the protected packets' bytes after the fixed header, as far
    /// as the protection length reaches.
    body: Vec<u8>,
}

/// An FEC packet taken apart, nothing copied but the numbers.
struct FecParts<'a> {
    /// The FEC header as far as the end of its mask.
    header: &'a [u8],
    /// The bytes it protects with.
    body: &'a [u8],
    /// The sequence numbers its mask names, in mask order.
    protected: Vec<u16>,
}

/// Take an FEC packet apart (the bytes after the RED header byte).
fn split_fec(fec: &[u8]) -> Result<FecParts<'_>, FecError> {
    if fec.len() > MAX_PACKET_LEN {
        return Err(FecError::Oversized);
    }
    let first = *fec.first().ok_or(FecError::Truncated)?;
    let mask_len = if first & FEC_LONG_MASK_BIT == 0 { 2 } else { 6 };
    let header_len = FEC_MASK_OFFSET + mask_len;
    let header = fec.get(..header_len).ok_or(FecError::Truncated)?;
    let protection_len = usize::from(u16::from_be_bytes([header[10], header[11]]));
    if protection_len > MAX_PACKET_LEN - RTP_HEADER_LEN {
        return Err(FecError::Oversized);
    }
    let body = fec
        .get(header_len..header_len + protection_len)
        .ok_or(FecError::Inconsistent)?;
    let sn_base = u16::from_be_bytes([header[2], header[3]]);
    let mut protected = Vec::new();
    for (byte_idx, byte) in header[FEC_MASK_OFFSET..].iter().enumerate() {
        for bit_idx in 0..8u16 {
            if byte & (0x80 >> bit_idx) != 0 {
                let distance = u16::try_from(byte_idx * 8).unwrap_or(0) + bit_idx;
                protected.push(sn_base.wrapping_add(distance));
            }
        }
    }
    if protected.is_empty() {
        return Err(FecError::ProtectsNothing);
    }
    Ok(FecParts {
        header,
        body,
        protected,
    })
}

/// Read an FEC packet (the bytes after the RED header byte).
fn parse_fec(seq: u16, fec: &[u8]) -> Result<FecPacket, FecError> {
    let FecParts {
        header,
        body,
        protected,
    } = split_fec(fec)?;
    Ok(FecPacket {
        seq,
        sn_base: u16::from_be_bytes([header[2], header[3]]),
        protected,
        head: [header[0], header[1]],
        ts_recovery: [header[4], header[5], header[6], header[7]],
        length_recovery: u16::from_be_bytes([header[8], header[9]]),
        body: body.to_vec(),
    })
}

/// The sequence numbers an FEC packet protects, in mask order (`fec` is what
/// follows the RED header byte). The SFU reads them off a publisher's FEC
/// packet to know which packets its own FEC packet for a subscriber's leg
/// has to cover ([`crate::fec_send`]), and which sequence numbers that are
/// missing on the publisher's stream were media ([`crate::burst_keyframe`]).
///
/// Reads the FEC header and nothing of the protected bytes, and copies
/// none of them.
///
/// # Errors
///
/// A [`FecError`] when the packet is malformed or protects nothing.
pub fn fec_protected(fec: &[u8]) -> Result<Vec<u16>, FecError> {
    split_fec(fec).map(|parts| parts.protected)
}

/// ULPFEC receive state for one publisher stream (one SSRC).
#[derive(Debug)]
pub struct FecReceiver {
    ssrc: u32,
    /// Media packets by sequence number modulo the window. A slot holds a
    /// packet only while its sequence number is within the window.
    media: Vec<Option<(u16, Vec<u8>)>>,
    fec: VecDeque<FecPacket>,
    /// The newest sequence number seen, media or FEC.
    newest: Option<u16>,
    /// Packets in a row that were older than the window.
    stale_run: u32,
}

impl FecReceiver {
    /// A receiver for the stream `ssrc`, holding nothing.
    #[must_use]
    pub fn new(ssrc: u32) -> Self {
        Self {
            ssrc,
            media: vec![None; usize::from(MEDIA_WINDOW)],
            fec: VecDeque::new(),
            newest: None,
            stale_run: 0,
        }
    }

    /// Media packets held now. At most [`MEDIA_WINDOW`].
    #[must_use]
    pub fn media_held(&self) -> usize {
        self.media.iter().flatten().count()
    }

    /// FEC packets held now. At most [`MAX_FEC_PACKETS`].
    #[must_use]
    pub fn fec_held(&self) -> usize {
        self.fec.len()
    }

    fn slot(seq: u16) -> usize {
        usize::from(seq % MEDIA_WINDOW)
    }

    fn holds(&self, seq: u16) -> bool {
        matches!(self.media.get(Self::slot(seq)), Some(Some((held, _))) if *held == seq)
    }

    fn packet(&self, seq: u16) -> Option<&[u8]> {
        match self.media.get(Self::slot(seq)) {
            Some(Some((held, packet))) if *held == seq => Some(packet),
            _ => None,
        }
    }

    fn clear(&mut self) {
        for slot in &mut self.media {
            *slot = None;
        }
        self.fec.clear();
    }

    /// Move the window to cover `seq`. `false` when `seq` is older than the
    /// window and nothing is to be done with the packet.
    fn admit(&mut self, seq: u16) -> bool {
        let Some(newest) = self.newest else {
            self.newest = Some(seq);
            return true;
        };
        let ahead = seq.wrapping_sub(newest);
        if ahead < 0x8000 {
            self.stale_run = 0;
            if ahead >= MEDIA_WINDOW {
                self.clear();
            } else {
                // Each number the window moves onto pushes the one a
                // window's length behind it out.
                for step in 1..=ahead {
                    let slot = Self::slot(newest.wrapping_add(step));
                    if let Some(held) = self.media.get_mut(slot) {
                        *held = None;
                    }
                }
                self.fec
                    .retain(|f| seq.wrapping_sub(f.sn_base) < MEDIA_WINDOW);
            }
            self.newest = Some(seq);
            return true;
        }
        if newest.wrapping_sub(seq) < MEDIA_WINDOW {
            self.stale_run = 0;
            return true;
        }
        self.stale_run += 1;
        if self.stale_run >= STALE_RESET {
            self.clear();
            self.stale_run = 0;
            self.newest = Some(seq);
            return true;
        }
        false
    }

    fn store(&mut self, seq: u16, packet: Vec<u8>) {
        if let Some(slot) = self.media.get_mut(Self::slot(seq)) {
            *slot = Some((seq, packet));
        }
    }

    /// Take a media packet with sequence number `seq`, in the form
    /// [`media_from_red`] produces, and return every packet that can now be
    /// rebuilt. A duplicate, a packet older than the window and a packet
    /// that is not RTP or too long are ignored.
    pub fn insert_media(&mut self, seq: u16, packet: Vec<u8>) -> Vec<Recovered> {
        if packet.len() < RTP_HEADER_LEN || packet.len() > MAX_PACKET_LEN {
            return Vec::new();
        }
        if !self.admit(seq) || self.holds(seq) {
            return Vec::new();
        }
        self.store(seq, packet);
        self.recover()
    }

    /// Take an FEC packet that arrived under sequence number `seq` (`fec` is
    /// what follows the RED header byte) and return every packet that can
    /// now be rebuilt.
    ///
    /// # Errors
    ///
    /// A [`FecError`] when the packet is malformed or of no use; nothing is
    /// stored then.
    pub fn insert_fec(&mut self, seq: u16, fec: &[u8]) -> Result<Vec<Recovered>, FecError> {
        let parsed = parse_fec(seq, fec)?;
        if !self.admit(seq) || self.fec.iter().any(|f| f.seq == seq) {
            return Err(FecError::NotNeeded);
        }
        let newest = self.newest.unwrap_or(seq);
        if newest.wrapping_sub(parsed.sn_base) >= MEDIA_WINDOW {
            // Protects packets the window no longer covers (or, for a base
            // ahead of everything seen, packets that cannot exist yet).
            return Err(FecError::NotNeeded);
        }
        if self.fec.len() >= MAX_FEC_PACKETS {
            self.fec.pop_front();
        }
        self.fec.push_back(parsed);
        Ok(self.recover())
    }

    /// Rebuild what can be rebuilt, repeating while a rebuilt packet makes
    /// another FEC packet usable.
    fn recover(&mut self) -> Vec<Recovered> {
        let mut out = Vec::new();
        loop {
            let mut progressed = false;
            let mut idx = 0;
            while idx < self.fec.len() {
                let mut missing = self
                    .fec
                    .get(idx)
                    .into_iter()
                    .flat_map(|f| f.protected.iter().copied())
                    .filter(|seq| !self.holds(*seq));
                let (first, second) = (missing.next(), missing.next());
                drop(missing);
                match (first, second) {
                    // More than one missing: keep it for later.
                    (Some(_), Some(_)) => idx += 1,
                    // Nothing missing: no further use.
                    (None, _) => {
                        let _ = self.fec.remove(idx);
                    },
                    (Some(seq), None) => {
                        let Some(fec) = self.fec.remove(idx) else {
                            break;
                        };
                        if let Some(packet) = self
                            .rebuild(&fec, seq)
                            .filter(|packet| self.fits_its_neighbours(seq, packet))
                        {
                            // The rebuilt packet may lie ahead of everything
                            // seen so far; move the window like an arrival.
                            if self.admit(seq) && !self.holds(seq) {
                                self.store(seq, packet.clone());
                                out.push(Recovered { seq, packet });
                                progressed = true;
                            }
                        }
                    },
                }
            }
            if !progressed {
                return out;
            }
        }
    }

    /// Whether a rebuilt packet's RTP timestamp lies between those of the
    /// nearest held packets before and after it.
    ///
    /// A packet rebuilt from inputs that are not what the sender's FEC covered
    /// is noise, and its timestamp is noise too. Forwarding it would be worse
    /// than the loss: the sequence number would count as received and never be
    /// asked for again. Sequence order and timestamp order agree within one
    /// stream, so a timestamp outside its neighbours' is refused and the
    /// packet stays missing. With no held neighbour on a side, that side is
    /// not checked.
    fn fits_its_neighbours(&self, seq: u16, packet: &[u8]) -> bool {
        let time_of = |p: &[u8]| {
            p.get(4..8)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(u32::from_be_bytes)
        };
        let Some(time) = time_of(packet) else {
            return false;
        };
        let reach = u16::try_from(MAX_PROTECTED).unwrap_or(u16::MAX);
        let before = (1..=reach)
            .find_map(|d| self.packet(seq.wrapping_sub(d)))
            .and_then(time_of);
        let after = (1..=reach)
            .find_map(|d| self.packet(seq.wrapping_add(d)))
            .and_then(time_of);
        let not_before = |earlier: u32, later: u32| later.wrapping_sub(earlier) < 0x8000_0000;
        before.map_or(true, |b| not_before(b, time)) && after.map_or(true, |a| not_before(time, a))
    }

    /// RFC 5109 section 9.2: the one protected packet not held, from the FEC
    /// packet and the protected packets that are held.
    ///
    /// The XOR runs over header fields and over whole packet tails. The tail
    /// holds `SFrame` ciphertext; it is combined, never read (C-39).
    fn rebuild(&self, fec: &FecPacket, missing: u16) -> Option<Vec<u8>> {
        let mut head = fec.head;
        let mut ts = fec.ts_recovery;
        let mut length = fec.length_recovery;
        let mut body = fec.body.clone();
        for seq in fec.protected.iter().copied().filter(|s| *s != missing) {
            let packet = self.packet(seq)?;
            let fixed = packet.get(..RTP_HEADER_LEN)?;
            let tail = packet.get(RTP_HEADER_LEN..)?;
            head[0] ^= fixed[0];
            head[1] ^= fixed[1];
            for (t, b) in ts.iter_mut().zip(&fixed[4..8]) {
                *t ^= b;
            }
            length ^= u16::try_from(tail.len()).ok()?;
            if tail.len() > body.len() {
                // A held packet longer than the protection length: the
                // sender's FEC would have covered it, so the inputs are not
                // what it was computed over. libwebrtc XORs on into zeroes;
                // so does this, and the length check below decides.
                body.resize(tail.len(), 0);
            }
            for (x, b) in body.iter_mut().zip(tail) {
                *x ^= b;
            }
        }
        let length = usize::from(length);
        if RTP_HEADER_LEN + length > MAX_PACKET_LEN - RTP_HEADER_LEN {
            return None;
        }
        body.resize(length, 0);
        let mut packet = Vec::with_capacity(RTP_HEADER_LEN + length);
        // RTP version 2 in place of the E and L bits.
        packet.push((head[0] | 0x80) & 0xbf);
        packet.push(head[1]);
        packet.extend_from_slice(&missing.to_be_bytes());
        packet.extend_from_slice(&ts);
        packet.extend_from_slice(&self.ssrc.to_be_bytes());
        packet.extend_from_slice(&body);
        Some(packet)
    }
}

#[cfg(test)]
// Test data is generated from counters; the low bits are the point.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    const SSRC: u32 = 0x1122_3344;
    const VP8: u8 = 96;
    const ULPFEC: u8 = 118;

    /// Extension ids as libwebrtc numbers them in these tests.
    const MUTABLE: MutableExtensions = MutableExtensions {
        abs_send_time: Some(2),
        transport_cc: Some(3),
        transmission_offset: None,
        video_timing: None,
    };

    /// A media packet in the form the encoder covers: a one-byte-form
    /// extension block with abs-send-time (zero), transport-cc (zero) and a
    /// `mid`, then `payload`.
    fn media(seq: u16, time: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x90, (u8::from(marker) << 7) | VP8];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&time.to_be_bytes());
        p.extend_from_slice(&SSRC.to_be_bytes());
        // 0xBEDE, 3 words: [2:3 bytes][3:2 bytes][4:1 byte "0"] + padding.
        p.extend_from_slice(&[0xbe, 0xde, 0x00, 0x03]);
        p.extend_from_slice(&[0x22, 0, 0, 0, 0x31, 0, 0, 0x40, b'0', 0, 0, 0]);
        p.extend_from_slice(payload);
        p
    }

    /// The same packet as it is on the wire: under `red_pt`, RED header byte
    /// in front of the payload, and the two mutable extensions filled in.
    fn on_the_wire(packet: &[u8], red_pt: u8, send_time: [u8; 3], twcc: u16) -> Vec<u8> {
        let headers = rtp_headers_len(packet).expect("rtp");
        let mut w = packet[..headers].to_vec();
        w[1] = (w[1] & 0x80) | red_pt;
        w[17..20].copy_from_slice(&send_time);
        w[21..23].copy_from_slice(&twcc.to_be_bytes());
        w.push(packet[1] & 0x7f);
        w.extend_from_slice(&packet[headers..]);
        w
    }

    /// An ULPFEC encoder written from RFC 5109 sections 7.3, 7.4 and 9.1 and
    /// from libwebrtc's `GenerateFecPayloads` / `FinalizeFecHeader`: one
    /// protection level, a 16-bit or 48-bit mask, the protection length set
    /// to the longest protected tail.
    fn encode(sn_base: u16, packets: &[&[u8]], long_mask: bool) -> Vec<u8> {
        let mask_len = if long_mask { 6 } else { 2 };
        let header_len = 12 + mask_len;
        let longest = packets
            .iter()
            .map(|p| p.len() - RTP_HEADER_LEN)
            .max()
            .unwrap_or(0);
        let mut fec = vec![0u8; header_len + longest];
        let mut length: u16 = 0;
        let mut mask = [0u8; 6];
        for p in packets {
            let seq = u16::from_be_bytes([p[2], p[3]]);
            let distance = usize::from(seq.wrapping_sub(sn_base));
            assert!(distance < mask_len * 8, "outside the mask");
            mask[distance / 8] |= 0x80 >> (distance % 8);
            fec[0] ^= p[0];
            fec[1] ^= p[1];
            for i in 4..8 {
                fec[i] ^= p[i];
            }
            let tail = &p[RTP_HEADER_LEN..];
            length ^= u16::try_from(tail.len()).unwrap();
            for (x, b) in fec[header_len..].iter_mut().zip(tail) {
                *x ^= b;
            }
        }
        fec[0] &= 0x7f; // E
        if long_mask {
            fec[0] |= 0x40;
        } else {
            fec[0] &= 0xbf;
        }
        fec[2..4].copy_from_slice(&sn_base.to_be_bytes());
        fec[8..10].copy_from_slice(&length.to_be_bytes());
        fec[10..12].copy_from_slice(&u16::try_from(longest).unwrap().to_be_bytes());
        fec[12..12 + mask_len].copy_from_slice(&mask[..mask_len]);
        fec
    }

    /// `n` media packets from `first`, of differing lengths, the last with
    /// the marker bit.
    fn group(first: u16, n: u16, time: u32) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                let seq = first.wrapping_add(i);
                let payload: Vec<u8> = (0..20 + usize::from(i) * 13)
                    .map(|b| (b as u8).wrapping_mul(31).wrapping_add(seq as u8))
                    .collect();
                media(seq, time, i + 1 == n, &payload)
            })
            .collect()
    }

    fn refs(packets: &[Vec<u8>]) -> Vec<&[u8]> {
        packets.iter().map(Vec::as_slice).collect()
    }

    fn seq_of(packet: &[u8]) -> u16 {
        u16::from_be_bytes([packet[2], packet[3]])
    }

    #[test]
    fn one_lost_packet_is_recovered_byte_for_byte() {
        for long_mask in [false, true] {
            let packets = group(1000, 4, 90_000);
            let fec = encode(1000, &refs(&packets), long_mask);
            for lost in 0..packets.len() {
                let mut rx = FecReceiver::new(SSRC);
                for (i, p) in packets.iter().enumerate() {
                    if i != lost {
                        assert!(rx.insert_media(seq_of(p), p.clone()).is_empty());
                    }
                }
                let got = rx.insert_fec(1004, &fec).expect("taken");
                assert_eq!(
                    got,
                    vec![Recovered {
                        seq: seq_of(&packets[lost]),
                        packet: packets[lost].clone()
                    }],
                    "long_mask={long_mask} lost={lost}"
                );
                assert_eq!(rx.fec_held(), 0, "a used FEC packet is not kept");
            }
        }
    }

    #[test]
    fn the_recovered_header_fields_are_the_lost_packets() {
        let mut packets = group(500, 3, 129_456);
        // A different timestamp and no marker on the one that is lost; the
        // timestamps rise with the sequence numbers, as in a real stream.
        packets[0] = media(500, 123_456, true, b"the frame before");
        packets[1] = media(501, 126_456, false, b"a payload of its own length");
        let fec = encode(500, &refs(&packets), false);
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(500, packets[0].clone());
        let _ = rx.insert_media(502, packets[2].clone());
        let got = rx.insert_fec(503, &fec).expect("taken");
        assert_eq!(got.len(), 1);
        let p = &got[0].packet;
        assert_eq!(got[0].seq, 501);
        assert_eq!(seq_of(p), 501, "sequence number");
        assert_eq!(p[0], 0x90, "version 2, extension bit, no padding, no CSRC");
        assert_eq!(p[1] & 0x7f, VP8, "payload type");
        assert_eq!(p[1] & 0x80, 0, "marker");
        assert_eq!(
            u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
            126_456,
            "timestamp"
        );
        assert_eq!(u32::from_be_bytes([p[8], p[9], p[10], p[11]]), SSRC);
        assert_eq!(p.len(), packets[1].len(), "length");
        assert_eq!(p, &packets[1]);

        // And the marker when the lost packet had it.
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(500, packets[0].clone());
        let _ = rx.insert_media(501, packets[1].clone());
        let got = rx.insert_fec(503, &fec).expect("taken");
        assert_eq!(got[0].packet[1], 0x80 | VP8);
        assert_eq!(got[0].packet, packets[2]);
    }

    #[test]
    fn media_arriving_after_the_fec_packet_triggers_the_recovery() {
        let packets = group(10, 3, 1);
        let fec = encode(10, &refs(&packets), false);
        let mut rx = FecReceiver::new(SSRC);
        assert!(rx.insert_media(10, packets[0].clone()).is_empty());
        assert_eq!(rx.insert_fec(13, &fec), Ok(Vec::new()), "two missing");
        assert_eq!(rx.fec_held(), 1);
        let got = rx.insert_media(12, packets[2].clone());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].packet, packets[1]);
    }

    #[test]
    fn two_lost_under_one_fec_packet_are_not_recovered() {
        let packets = group(2000, 4, 7);
        let fec = encode(2000, &refs(&packets), false);
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(2000, packets[0].clone());
        let _ = rx.insert_media(2003, packets[3].clone());
        assert_eq!(rx.insert_fec(2004, &fec), Ok(Vec::new()));
        assert_eq!(rx.fec_held(), 1, "kept in case one of the two arrives");
        assert_eq!(rx.media_held(), 2, "nothing was made up");
    }

    #[test]
    fn nothing_missing_means_nothing_emitted() {
        let packets = group(1, 3, 7);
        let fec = encode(1, &refs(&packets), true);
        let mut rx = FecReceiver::new(SSRC);
        for p in &packets {
            let _ = rx.insert_media(seq_of(p), p.clone());
        }
        assert_eq!(rx.insert_fec(4, &fec), Ok(Vec::new()));
        assert_eq!(rx.fec_held(), 0);
    }

    #[test]
    fn one_recovery_enables_another() {
        // FEC 1 covers 100..=102, FEC 2 covers 102..=104. 101 and 102 are
        // lost with 104; FEC 2 is useless (102 and 104 missing) until... no:
        // lose 102 and 103. FEC 2 (102, 103, 104) has two missing; FEC 1
        // (100, 101, 102) has one. Rebuilding 102 from FEC 1 leaves FEC 2
        // with one missing, which rebuilds 103.
        let packets = group(100, 5, 9);
        let fec1 = encode(100, &refs(&packets[0..3]), false);
        let fec2 = encode(102, &refs(&packets[2..5]), false);
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(100, packets[0].clone());
        let _ = rx.insert_media(101, packets[1].clone());
        let _ = rx.insert_media(104, packets[4].clone());
        assert_eq!(rx.insert_fec(106, &fec2), Ok(Vec::new()));
        let got = rx.insert_fec(105, &fec1).expect("taken");
        assert_eq!(
            got.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![102, 103]
        );
        assert_eq!(got[0].packet, packets[2]);
        assert_eq!(got[1].packet, packets[3]);
        assert_eq!(rx.fec_held(), 0);
    }

    #[test]
    fn a_rebuilt_packet_with_a_timestamp_outside_its_neighbours_is_refused() {
        let packets = group(100, 4, 90_000);
        let mut fec = encode(100, &refs(&packets), false);
        // Damage the timestamp recovery field: the rebuilt packet would carry
        // a timestamp half the range away from every neighbour.
        fec[4] ^= 0x80;
        let mut rx = FecReceiver::new(SSRC);
        for (i, p) in packets.iter().enumerate() {
            if i != 1 {
                let _ = rx.insert_media(seq_of(p), p.clone());
            }
        }
        let got = rx.insert_fec(104, &fec).expect("taken");
        assert!(got.is_empty(), "a packet that does not fit is not emitted");
        assert!(!rx.holds(101), "and its sequence number stays missing");
    }

    #[test]
    fn recovery_works_across_the_sequence_wrap() {
        for long_mask in [false, true] {
            let packets = group(65_534, 4, 90_000);
            assert_eq!(seq_of(&packets[2]), 0);
            let fec = encode(65_534, &refs(&packets), long_mask);
            for lost in 0..packets.len() {
                let mut rx = FecReceiver::new(SSRC);
                // Something before the wrap, so the window has to move
                // across it.
                let before = group(65_500, 3, 1);
                for p in &before {
                    let _ = rx.insert_media(seq_of(p), p.clone());
                }
                for (i, p) in packets.iter().enumerate() {
                    if i != lost {
                        let _ = rx.insert_media(seq_of(p), p.clone());
                    }
                }
                let got = rx.insert_fec(2, &fec).expect("taken");
                assert_eq!(got.len(), 1, "lost={lost}");
                assert_eq!(got[0].packet, packets[lost]);
            }
        }
    }

    #[test]
    fn a_wide_mask_reaches_the_forty_eighth_packet() {
        let packets = group(300, 48, 5);
        let fec = encode(300, &refs(&packets), true);
        let mut rx = FecReceiver::new(SSRC);
        for p in &packets[..47] {
            let _ = rx.insert_media(seq_of(p), p.clone());
        }
        let got = rx.insert_fec(348, &fec).expect("taken");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].packet, packets[47]);
    }

    #[test]
    fn duplicates_and_late_packets_are_ignored() {
        let packets = group(40, 3, 5);
        let fec = encode(40, &refs(&packets), false);
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(40, packets[0].clone());
        // The same number again with other bytes: the first one stays.
        let _ = rx.insert_media(40, media(40, 5, false, b"an impostor"));
        let _ = rx.insert_media(42, packets[2].clone());
        let got = rx.insert_fec(43, &fec).expect("taken");
        assert_eq!(got[0].packet, packets[1]);
        // The FEC packet again, and the recovered packet arriving after all.
        assert_eq!(rx.insert_fec(43, &fec), Ok(Vec::new()));
        assert!(rx.insert_media(41, packets[1].clone()).is_empty());

        // A packet far behind the window.
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(5000, media(5000, 1, false, b"x"));
        assert!(rx
            .insert_media(4000, media(4000, 1, false, b"y"))
            .is_empty());
        assert_eq!(rx.media_held(), 1);
        assert_eq!(rx.insert_fec(4001, &fec), Err(FecError::NotNeeded));
        // An FEC packet in the window whose base is not.
        let old = encode(4000, &[&media(4000, 1, false, b"y")], false);
        assert_eq!(rx.insert_fec(5001, &old), Err(FecError::NotNeeded));
    }

    #[test]
    fn a_stream_that_restarts_its_numbering_is_followed() {
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(30_000, media(30_000, 1, false, b"x"));
        for i in 0..STALE_RESET as u16 {
            let seq = 10_000 + i;
            let _ = rx.insert_media(seq, media(seq, 1, false, b"y"));
        }
        assert!(rx.holds(10_000 + STALE_RESET as u16 - 1));
        assert!(!rx.holds(30_000));
    }

    #[test]
    fn malformed_fec_headers_are_refused() {
        let packets = group(1, 2, 5);
        for long_mask in [false, true] {
            let fec = encode(1, &refs(&packets), long_mask);
            let header_len = if long_mask { 18 } else { 14 };
            let mut rx = FecReceiver::new(SSRC);
            for len in 0..header_len {
                assert_eq!(
                    rx.insert_fec(3, &fec[..len]),
                    Err(FecError::Truncated),
                    "long_mask={long_mask} len={len}"
                );
            }
            // Shorter than the protection length it states.
            for len in header_len..fec.len() {
                assert_eq!(rx.insert_fec(3, &fec[..len]), Err(FecError::Inconsistent));
            }
            // A protection length no packet has.
            let mut big = fec.clone();
            big[10..12].copy_from_slice(&u16::MAX.to_be_bytes());
            assert_eq!(rx.insert_fec(3, &big), Err(FecError::Oversized));
            // Longer than any packet.
            let mut long = fec.clone();
            long.resize(MAX_PACKET_LEN + 1, 0);
            assert_eq!(rx.insert_fec(3, &long), Err(FecError::Oversized));
            // An empty mask.
            let mut empty = fec.clone();
            for b in &mut empty[12..header_len] {
                *b = 0;
            }
            assert_eq!(rx.insert_fec(3, &empty), Err(FecError::ProtectsNothing));
            assert_eq!(rx.fec_held(), 0, "nothing malformed is stored");
        }
    }

    #[test]
    fn the_protected_numbers_are_read_off_the_mask() {
        let packets = group(65_534, 4, 5);
        let sparse = [packets[0].as_slice(), packets[3].as_slice()];
        assert_eq!(
            fec_protected(&encode(65_534, &sparse, false)),
            Ok(vec![65_534, 1])
        );
        let all = group(300, 48, 5);
        assert_eq!(
            fec_protected(&encode(300, &refs(&all), true)),
            Ok((300..348).collect::<Vec<u16>>())
        );
        assert_eq!(fec_protected(&[0; 13]), Err(FecError::Truncated));
        assert_eq!(fec_protected(&[0; 14]), Err(FecError::ProtectsNothing));
    }

    #[test]
    fn a_length_recovery_that_cannot_be_a_packet_rebuilds_nothing() {
        let packets = group(1, 2, 5);
        let mut fec = encode(1, &refs(&packets), false);
        // Flip the high bits of the length recovery field.
        fec[8] ^= 0xff;
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(1, packets[0].clone());
        assert_eq!(rx.insert_fec(3, &fec), Ok(Vec::new()));
        assert_eq!(rx.media_held(), 1);
        assert_eq!(rx.fec_held(), 0, "an FEC packet is tried once");
    }

    #[test]
    fn the_window_bound_holds() {
        let mut rx = FecReceiver::new(SSRC);
        // Media on every other number, an unusable FEC packet on the rest,
        // over two full cycles of the sequence space.
        let mut seq: u16 = 60_000;
        for _ in 0..140_000u32 {
            if seq % 2 == 0 {
                let _ = rx.insert_media(seq, media(seq, 1, false, &[seq as u8; 40]));
            } else {
                // Protects two numbers that never arrive as media: they are
                // odd, and odd numbers carry FEC here.
                let a = media(seq.wrapping_sub(4), 1, false, b"a");
                let b = media(seq.wrapping_sub(2), 1, false, b"b");
                let fec = encode(seq.wrapping_sub(4), &[&a, &b], false);
                let _ = rx.insert_fec(seq, &fec);
            }
            assert!(rx.media_held() <= usize::from(MEDIA_WINDOW));
            assert!(rx.fec_held() <= MAX_FEC_PACKETS);
            seq = seq.wrapping_add(1);
        }
        assert_eq!(rx.media_held(), usize::from(MEDIA_WINDOW) / 2);
        assert_eq!(rx.fec_held(), MAX_FEC_PACKETS);
        // Every held packet is within the window.
        let newest = rx.newest.expect("packets were seen");
        for (held, _) in rx.media.iter().flatten() {
            assert!(newest.wrapping_sub(*held) < MEDIA_WINDOW);
        }
        // A jump far ahead drops everything held.
        let _ = rx.insert_media(
            newest.wrapping_add(5000),
            media(newest.wrapping_add(5000), 1, false, b"x"),
        );
        assert_eq!(rx.media_held(), 1);
        assert_eq!(rx.fec_held(), 0);
        // Oversized media is not stored.
        let far = newest.wrapping_add(5001);
        let _ = rx.insert_media(far, vec![0x80; MAX_PACKET_LEN + 1]);
        assert_eq!(rx.media_held(), 1);
    }

    #[test]
    fn a_stale_slot_is_never_read_as_the_packet_a_window_later() {
        // 7 is held; the stream moves on a whole window without 7 + 256
        // arriving. An FEC packet protecting 7 + 256 must see it as missing
        // and must not XOR the packet numbered 7.
        let mut rx = FecReceiver::new(SSRC);
        let _ = rx.insert_media(7, media(7, 1, false, b"old"));
        // Steps shorter than the window, so nothing is dropped wholesale.
        for seq in [100, 200] {
            let _ = rx.insert_media(seq, media(seq, 1, false, b"between"));
        }
        let later = group(7 + MEDIA_WINDOW, 3, 9);
        let fec = encode(7 + MEDIA_WINDOW, &refs(&later), false);
        let _ = rx.insert_media(seq_of(&later[1]), later[1].clone());
        let _ = rx.insert_media(seq_of(&later[2]), later[2].clone());
        let got = rx.insert_fec(7 + MEDIA_WINDOW + 3, &fec).expect("taken");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].packet, later[0]);
    }

    #[test]
    fn the_wire_form_becomes_the_form_fec_covers() {
        let packet = media(77, 3000, true, b"sealed frame bytes");
        let wire = on_the_wire(&packet, 116, [0xaa, 0xbb, 0xcc], 0x1234);
        assert_ne!(wire, packet);
        assert_eq!(media_from_red(&wire, &MUTABLE), Some(packet.clone()));
        // Without the ids the extensions stay as sent.
        let kept = media_from_red(&wire, &MutableExtensions::default()).expect("rtp");
        assert_eq!(&kept[17..20], &[0xaa, 0xbb, 0xcc]);
        assert_eq!(&kept[21..23], &[0x12, 0x34]);
        // `mid` (id 4) is never touched.
        assert_eq!(kept[24], b'0');
        assert_eq!(packet[24], b'0');
    }

    #[test]
    fn packets_recovered_from_the_wire_form_match() {
        // The whole path: FEC computed over the pre-pacer form, packets
        // arriving in the wire form with different mutable values each.
        let packets = group(900, 3, 42);
        let fec = encode(900, &refs(&packets), false);
        let mut rx = FecReceiver::new(SSRC);
        for (i, p) in packets.iter().enumerate().skip(1) {
            let wire = on_the_wire(p, 116, [i as u8, 2, 3], 700 + i as u16);
            let view = media_from_red(&wire, &MUTABLE).expect("rtp");
            let _ = rx.insert_media(seq_of(p), view);
        }
        let mut red_fec = vec![ULPFEC];
        red_fec.extend_from_slice(&fec);
        let got = rx.insert_fec(903, &red_fec[1..]).expect("taken");
        assert_eq!(got[0].packet, packets[0]);
    }

    #[test]
    fn two_byte_form_extensions_and_video_timing_are_zeroed() {
        // Two-byte form: [id 2, len 3][id 9, len 13 video-timing][id 4, len 1].
        let mut p = vec![0x90, VP8, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        p.extend_from_slice(&[0x10, 0x00, 0x00, 0x06]);
        p.extend_from_slice(&[2, 3, 0xaa, 0xbb, 0xcc]);
        p.extend_from_slice(&[9, 13]);
        p.extend_from_slice(&[0xee; 13]);
        p.extend_from_slice(&[4, 1, b'0']);
        p.push(0);
        assert_eq!(p.len(), 12 + 4 + 24);
        p.extend_from_slice(b"payload");
        let mut zeroed = p.clone();
        zero_mutable_extensions(
            &mut zeroed,
            &MutableExtensions {
                abs_send_time: Some(2),
                video_timing: Some(9),
                ..MutableExtensions::default()
            },
        );
        assert_eq!(&zeroed[18..21], &[0, 0, 0]);
        assert_eq!(&zeroed[23..30], &[0xee; 7], "first seven bytes kept");
        assert_eq!(&zeroed[30..36], &[0; 6], "the rest zeroed");
        assert_eq!(&zeroed[36..39], &[4, 1, b'0']);
        assert_eq!(&zeroed[40..], b"payload");
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let mutable = MutableExtensions {
            abs_send_time: Some(1),
            transport_cc: Some(2),
            transmission_offset: Some(15),
            video_timing: Some(255),
        };
        let mut rx = FecReceiver::new(SSRC);
        let mut state: u32 = 0x9e37_79b9;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for round in 0..20_000u32 {
            let len = (next() % 80) as usize;
            let mut bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if round % 3 == 0 && len > 0 {
                // Often enough, something that passes the version check.
                bytes[0] = 0x80 | (bytes[0] & 0x3f);
            }
            let seq = (next() % 600) as u16;
            let _ = rtp_headers_len(&bytes);
            if let Some(view) = media_from_red(&bytes, &mutable) {
                for r in rx.insert_media(seq, view) {
                    assert!(r.packet.len() >= RTP_HEADER_LEN);
                    assert!(r.packet.len() <= MAX_PACKET_LEN);
                }
            }
            if let Ok(recovered) = rx.insert_fec(seq, &bytes) {
                for r in recovered {
                    assert_eq!(r.packet[0] >> 6, 2);
                    assert_eq!(seq_of(&r.packet), r.seq);
                }
            }
            let mut copy = bytes.clone();
            zero_mutable_extensions(&mut copy, &mutable);
            assert_eq!(copy.len(), bytes.len());
            assert!(rx.media_held() <= usize::from(MEDIA_WINDOW));
            assert!(rx.fec_held() <= MAX_FEC_PACKETS);
        }
    }
}
