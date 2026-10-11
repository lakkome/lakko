// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — video RED handling, the ULPFEC receiver and the ULPFEC
//! sender (`services/sfu/src/video_fec.rs`, `services/sfu/src/fec_receive.rs`,
//! `services/sfu/src/fec_send.rs`, ADR-0067 step 4).
//!
//! With `LAKKO_SFU_VIDEO_FEC` on, the media loop reads the RED header byte
//! of every video packet a publisher sends under the `red` payload type,
//! gives every such media packet (headers included) and every FEC packet to
//! the stream's FEC receiver, and forwards what the receiver rebuilds. Every
//! byte is chosen by an admitted peer and reaches this code before anything
//! else looks at it. A panic stops the media task, and with it every call.
//!
//! The input is read as a sequence of packets for one stream:
//!
//! ```text
//! [kind and sequence step: 1 byte] [length: 1 byte] [that many bytes] ...
//! ```
//!
//! The low bit of the first byte chooses media (a whole RTP packet as
//! received, RED-wrapped) or FEC (the bytes after the RED header byte); the
//! rest of it moves the sequence number, forwards or backwards, so the
//! window, the wrap and the stale paths are all reachable. The first four
//! input bytes are the header extension ids to zero.
//!
//! The same packets also drive the sender the SFU keeps per subscriber leg:
//! what a publisher sends is what the SFU serializes for a subscriber (the
//! payload verbatim, the lengths the publisher's), and the mask of a
//! publisher's FEC packet is what the SFU asks its own sender to protect. A
//! media packet is recorded as sent; an FEC packet's protected numbers
//! become a request for the slot the packet sits in.
//!
//! The same packets also drive the bookkeeping that says when a burst of
//! loss on a publisher's stream has ended unrepaired
//! (`services/sfu/src/burst_keyframe.rs`, ADR-0070 note of 2026-10-11): the
//! sequence number of each packet, the numbers an FEC packet's mask names
//! as protected, and, for a media packet, where its frame descriptor says
//! it sits in its frame (the `B` and `E` flags and the frame id, read off
//! the last bytes of the packet as a descriptor value). An admitted peer
//! chooses all three. The packets are spread over more streams than the
//! bookkeeping keeps, and its clock moves by a step the input chooses.
//!
//! Beyond not panicking, asserted after every packet:
//!
//! - the burst bookkeeping keeps no more streams and no more missing
//!   packets than its stated bounds, and every burst it hands out counts
//!   its missing packets consistently (known media, known not media and
//!   unknown add up to what never arrived, which is no more than what was
//!   lost);
//!
//! - the sender holds no more than its stated bounds, and an FEC packet it
//!   builds is no longer than its stated bound and no shorter than an FEC
//!   header with a mask;
//! - the protected numbers read off an FEC packet are at most 48 and are a
//!   list the sender's mask builder accepts (the two agree on what a mask
//!   is);
//! - the receiver holds no more than its stated bounds;
//! - a rebuilt packet is RTP version 2, carries the sequence number it was
//!   rebuilt for, and is no longer than the longest packet the receiver
//!   handles;
//! - a rewrapped RED payload keeps its body and its length.

#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;

use lakko_svc_sfu::burst_keyframe::{Arrived, BurstEnds, MAX_STREAMS, RETRANSMISSION_WINDOW};
use lakko_svc_sfu::frame_descriptor::packet_position;

use lakko_svc_sfu::fec_receive::{
    fec_protected, media_from_red, rtp_headers_len, zero_mutable_extensions, FecReceiver,
    MutableExtensions, MAX_FEC_PACKETS, MAX_PACKET_LEN, MAX_PROTECTED, MEDIA_WINDOW,
    RTP_HEADER_LEN,
};
use lakko_svc_sfu::fec_send::{self, mask_for, FecSender, MAX_FEC_LEN, MAX_REQUESTS};
use lakko_svc_sfu::video_fec::{red_block_pt, red_body, red_rewrap};

fn id(byte: u8) -> Option<u8> {
    (byte != 0).then_some(byte)
}

fuzz_target!(|data: &[u8]| {
    let Some((head, mut rest)) = data.split_first_chunk::<4>() else {
        return;
    };
    let mutable = MutableExtensions {
        abs_send_time: id(head[0]),
        transport_cc: id(head[1]),
        transmission_offset: id(head[2]),
        video_timing: id(head[3]),
    };
    let mut receiver = FecReceiver::new(0x0102_0304);
    let mut sender: FecSender<u16> = FecSender::new();
    let mut seq: u16 = 0xfff0;
    // The same number without the wrap; its low 16 bits are `seq`.
    let mut ext: u64 = 0x1_0000_fff0;
    let mut bursts: BurstEnds<u8> = BurstEnds::new();
    let start = Instant::now();
    let mut clock_ms: u64 = 0;

    while let Some((&[control, len], tail)) = rest.split_first_chunk::<2>() {
        let take = usize::from(len).min(tail.len());
        let (packet, tail) = tail.split_at(take);
        rest = tail;

        // Bits 1..8: a step of -16..=111, so the stream mostly advances,
        // sometimes repeats a number and sometimes goes back.
        let step = i16::from(control >> 1) - 16;
        seq = seq.wrapping_add_signed(step);
        ext = ext.saturating_add_signed(i64::from(step));
        if control >> 1 == 0x7f {
            // And now and then a jump far outside the window.
            seq = seq.wrapping_add(20_000);
            ext = ext.saturating_add(20_000);
        }
        // The burst bookkeeping: sixteen streams (it keeps twelve), a clock
        // that moves 0 to 140 ms a packet.
        let stream = len & 0x0f;
        clock_ms += u64::from(control & 0x0e) * 10;
        let now = start + Duration::from_millis(clock_ms);
        if control & 1 == 0 {
            let value = &packet[packet.len().saturating_sub(usize::from(len % 6))..];
            let part = packet_position(value);
            let what = if control & 0x10 == 0 {
                Arrived::Media
            } else {
                Arrived::KeyFrameStart
            };
            bursts.on_described(stream, ext, now, what, part);
        } else {
            bursts.on_packet(stream, ext, now, Arrived::Fec);
            if let Ok(protected) = fec_protected(packet) {
                bursts.fec_protects(stream, ext, &protected);
            }
        }
        for end in bursts.poll(now) {
            assert!(end.unrepaired > 0 && end.unrepaired <= end.lost);
            assert_eq!(end.unrepaired, end.media + end.not_media + end.unknown);
        }
        assert!(bursts.streams_kept() <= MAX_STREAMS);
        let most_holes = MAX_STREAMS * usize::try_from(RETRANSMISSION_WINDOW).unwrap_or(usize::MAX);
        assert!(bursts.open_holes() <= most_holes);

        // The RED header helpers see every payload.
        let _ = red_block_pt(packet);
        let body = red_body(packet);
        if let Some(out) = red_rewrap(packet, control & 0x7f) {
            assert_eq!(out.len(), packet.len());
            assert_eq!(Some(&out[1..]), body);
        }
        let _ = rtp_headers_len(packet);
        let mut copy = packet.to_vec();
        zero_mutable_extensions(&mut copy, &mutable);
        assert_eq!(copy.len(), packet.len());

        let mut generated = Vec::new();
        let rebuilt = if control & 1 == 0 {
            match media_from_red(packet, &mutable) {
                Some(view) => {
                    assert_eq!(view.len() + 1, packet.len());
                    generated = sender.sent_media(seq, view.clone());
                    receiver.insert_media(seq, view)
                },
                None => Vec::new(),
            }
        } else {
            if let Ok(protected) = fec_protected(packet) {
                assert!(!protected.is_empty() && protected.len() <= MAX_PROTECTED);
                assert!(mask_for(&protected).is_some());
                generated = sender.request(seq, protected, seq).unwrap_or_default();
            }
            receiver.insert_fec(seq, packet).unwrap_or_default()
        };
        for g in &generated {
            assert_eq!(g.slot, g.tag);
            assert!(g.fec.len() >= 14 && g.fec.len() <= MAX_FEC_LEN);
            // What the sender built is an FEC packet the receiver's parser
            // takes, protecting numbers a mask can name.
            let named = fec_protected(&g.fec).expect("a well-formed FEC packet");
            assert!(mask_for(&named).is_some());
        }
        assert!(sender.media_held() <= usize::from(fec_send::MEDIA_WINDOW));
        assert!(sender.requests_waiting() <= MAX_REQUESTS);
        for r in &rebuilt {
            assert!(r.packet.len() >= RTP_HEADER_LEN);
            assert!(r.packet.len() <= MAX_PACKET_LEN);
            assert_eq!(r.packet[0] >> 6, 2);
            assert_eq!(u16::from_be_bytes([r.packet[2], r.packet[3]]), r.seq);
        }
        assert!(receiver.media_held() <= usize::from(MEDIA_WINDOW));
        assert!(receiver.fec_held() <= MAX_FEC_PACKETS);
    }
});
