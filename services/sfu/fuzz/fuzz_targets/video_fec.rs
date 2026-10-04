// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — video RED / ULPFEC header handling
//! (`services/sfu/src/video_fec.rs`, ADR-0067 step 4).
//!
//! With `LAKKO_SFU_VIDEO_FEC` on, the media loop reads the RED header byte
//! of every video packet a publisher sends under the `red` payload type, and
//! the FEC header of every FEC packet. The payload is chosen byte for byte
//! by an admitted peer and reaches these functions before anything else
//! looks at it. A panic stops the media task, and with it every call.
//!
//! The input is split into a few bytes of mapping (the subscriber's payload
//! types and the stream offsets) and the RTP payload. Beyond not panicking,
//! three properties are asserted:
//!
//! - a rewrapped media packet keeps its body and its length;
//! - a forwarded FEC packet differs from the input in the RED header byte
//!   and the two `SN base` bytes at most, and its `SN base` is the input's
//!   plus the sequence offset;
//! - an FEC packet is never forwarded onto a stream whose timestamps are
//!   offset or whose media payload type differs.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_svc_sfu::video_fec::{
    forward_fec, parse_fec_header, red_block_pt, red_body, red_rewrap, FecMapping,
};

fuzz_target!(|data: &[u8]| {
    let Some((head, payload)) = data.split_first_chunk::<8>() else {
        return;
    };
    let block_pt = head[0];
    let mapping = FecMapping {
        seq_offset: u16::from_be_bytes([head[1], head[2]]),
        time_offset: u32::from(head[3] & 1),
        media_pt: match head[4] & 3 {
            0 => None,
            1 => Some((head[5], head[5])),
            _ => Some((head[5], head[6])),
        },
    };

    let _ = red_block_pt(payload);
    let body = red_body(payload);
    if let Some(out) = red_rewrap(payload, block_pt) {
        assert_eq!(out.len(), payload.len());
        assert_eq!(out[0], block_pt);
        assert_eq!(Some(&out[1..]), body);
    }

    let before = body.and_then(parse_fec_header);
    if let Ok(out) = forward_fec(payload, block_pt, &mapping) {
        let before = before.expect("a forwarded packet had a readable header");
        assert_eq!(mapping.time_offset, 0);
        assert!(matches!(mapping.media_pt, Some((a, b)) if a == b));
        assert_eq!(out.len(), payload.len());
        assert_eq!(out[0], block_pt);
        let after = parse_fec_header(&out[1..]).expect("still an FEC packet");
        assert_eq!(after.long_mask, before.long_mask);
        assert_eq!(after.sn_base, before.sn_base.wrapping_add(mapping.seq_offset));
        for (i, (a, b)) in out.iter().zip(payload).enumerate() {
            if !matches!(i, 0 | 3 | 4) {
                assert_eq!(a, b, "byte {i} changed");
            }
        }
    }
});
