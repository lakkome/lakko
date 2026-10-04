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
//! Beyond not panicking, asserted after every packet:
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

use libfuzzer_sys::fuzz_target;

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

    while let Some((&[control, len], tail)) = rest.split_first_chunk::<2>() {
        let take = usize::from(len).min(tail.len());
        let (packet, tail) = tail.split_at(take);
        rest = tail;

        // Bits 1..8: a step of -16..=111, so the stream mostly advances,
        // sometimes repeats a number and sometimes goes back.
        let step = i16::from(control >> 1) - 16;
        seq = seq.wrapping_add_signed(step);
        if control >> 1 == 0x7f {
            // And now and then a jump far outside the window.
            seq = seq.wrapping_add(20_000);
        }

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
